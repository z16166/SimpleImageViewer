use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use egui::{
    Align2, Color32, ColorImage, Context, FontId, Frame, Key, Pos2, Rect, RichText,
    Sense, TextureOptions, Vec2,
};

use crate::audio::{AudioPlayer, collect_music_files};
use crate::ipc::IpcMessage;
use crate::loader::{ImageData, ImageLoader, TextureCache};
use crate::scanner;
use crate::settings::{ScaleMode, Settings, TransitionStyle};
use crate::tile_cache::TileManager;

const PRELOAD_AHEAD: usize = 1;
const PRELOAD_BEHIND: usize = 1;
const CACHE_SIZE: usize = 4; // 1 current + PRELOAD_AHEAD + PRELOAD_BEHIND + 1 buffer

// Accent colors for the UI
const BG_DARK: Color32 = Color32::from_rgb(18, 18, 24);
const PANEL_BG: Color32 = Color32::from_rgb(32, 33, 36);
const ACCENT: Color32 = Color32::from_rgb(108, 92, 231);
const ACCENT2: Color32 = Color32::from_rgb(0, 199, 190);
const TEXT_MUTED: Color32 = Color32::from_rgb(154, 160, 166);

/// Animation playback state for the currently displayed animated image.
struct AnimationPlayback {
    /// Index in the image_files list that this animation belongs to.
    image_index: usize,
    /// Pre-uploaded GPU textures for each frame.
    textures: Vec<egui::TextureHandle>,
    /// Per-frame display duration.
    delays: Vec<Duration>,
    /// Currently displayed frame index.
    current_frame: usize,
    /// When the current frame started displaying.
    frame_start: Instant,
}

/// Parameters that affect the OSD status text.
#[derive(PartialEq)]
struct HudState {
    index: usize,
    total: usize,
    zoom_pct: u32,
    res: (u32, u32),
    mode: String,
    current_track: Option<String>,
}


pub struct ImageViewerApp {
    settings: Settings,
    save_tx: Sender<Settings>,
    initial_image: Option<PathBuf>,

    // File list
    image_files: Vec<PathBuf>,
    current_index: usize,

    // Channel receiving scanned file list
    scan_rx: Option<Receiver<Vec<PathBuf>>>,
    scanning: bool,

    // Image loading
    loader: ImageLoader,
    texture_cache: TextureCache,
    /// Animated image playback state (None for static images).
    animation: Option<AnimationPlayback>,

    // Pan/drag state (used in non-fullscreen 1:1 mode)
    pan_offset: Vec2,

    // Manual zoom factor (1.0 = 100%); applied on top of any fit-to-screen scale
    zoom_factor: f32,

    // Auto-switch timer
    last_switch_time: Instant,
    slideshow_paused: bool,

    // Audio
    audio: AudioPlayer,

    // UI state
    show_settings: bool,
    status_message: String,
    error_message: Option<String>,

    // Pending viewport commands (set during input processing for deferred apply)
    pending_fullscreen: Option<bool>,

    // Cached system font families
    font_families: Vec<String>,
    temp_font_size: Option<f32>,

    // Cached state
    generation: u64,
    cached_music_count: Option<usize>,
    cached_pixels_per_point: f32,

    // EXIF dialog state
    show_exif_window: bool,
    cached_exif_text: Option<String>,

    // XMP dialog state
    show_xmp_window: bool,
    cached_xmp_text: Option<String>,

    // Goto dialog state
    show_goto: bool,
    goto_input: String,
    goto_needs_focus: bool,

    // Async music scanning
    music_scan_rx: Option<Receiver<Vec<PathBuf>>>,
    scanning_music: bool,
    music_scan_cancel: Option<Arc<AtomicBool>>,
    music_scan_path: Option<PathBuf>,

    // Wallpaper dialog state
    show_wallpaper_dialog: bool,
    selected_wallpaper_mode: String,
    current_image_res: Option<(u32, u32)>,
    current_system_wallpaper: Option<String>,

    // File association dialog state (Windows only)
    #[cfg(target_os = "windows")]
    show_file_assoc_dialog: bool,
    #[cfg(target_os = "windows")]
    file_assoc_selections: Vec<bool>,

    // Transition state
    prev_texture: Option<egui::TextureHandle>,
    transition_start: Option<Instant>,
    is_next: bool,

    // Caching for OSD performance
    cached_hud: Option<String>,
    last_hud_state: Option<HudState>,

    // Window lifecycle
    last_minimized: bool,
    last_frame_time: Instant,

    // IPC receiver
    ipc_rx: crossbeam_channel::Receiver<IpcMessage>,
    
    // Predictive animation cache (decoded and uploaded to GPU)
    animation_cache: std::collections::HashMap<usize, AnimationPlayback>,

    // Tiled rendering for large images
    tile_manager: Option<TileManager>,
}

impl ImageViewerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        initial_image: Option<PathBuf>,
        ipc_rx: crossbeam_channel::Receiver<IpcMessage>,
    ) -> Self {
        if settings.fullscreen {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        setup_visuals(&cc.egui_ctx, &settings);
        setup_fonts(&cc.egui_ctx, &settings);

        let (save_tx, save_rx) = crossbeam_channel::unbounded::<Settings>();
        std::thread::Builder::new()
            .name("settings-saver".to_string())
            .spawn(move || {
                while let Ok(settings) = save_rx.recv() {
                    settings.save();
                }
            })
            .expect("failed to spawn settings saver thread");

        let mut app = Self {
            settings,
            save_tx,
            initial_image,
            image_files: Vec::new(),
            current_index: 0,
            scan_rx: None,
            scanning: false,
            loader: ImageLoader::new(),
            texture_cache: TextureCache::new(CACHE_SIZE),
            animation: None,
            pan_offset: Vec2::ZERO,
            zoom_factor: 1.0,
            last_switch_time: Instant::now(),
            slideshow_paused: false,
            audio: AudioPlayer::new(),
            show_settings: true,
            status_message: "Open a directory to start viewing images".to_string(),
            error_message: None,
            pending_fullscreen: None,
            font_families: get_system_font_families(),
            temp_font_size: None,
            generation: 0,
            cached_music_count: None,
            cached_pixels_per_point: 1.0,
            show_exif_window: false,
            cached_exif_text: None,
            show_xmp_window: false,
            cached_xmp_text: None,
            show_goto: false,
            goto_input: String::new(),
            goto_needs_focus: false,
            music_scan_rx: None,
            scanning_music: false,
            music_scan_cancel: None,
            music_scan_path: None,
            show_wallpaper_dialog: false,
            selected_wallpaper_mode: "Crop".to_string(),
            current_image_res: None,
            current_system_wallpaper: None,
            #[cfg(target_os = "windows")]
            show_file_assoc_dialog: false,
            #[cfg(target_os = "windows")]
            file_assoc_selections: vec![true; scanner::SUPPORTED_EXTENSIONS.len()],
            prev_texture: None,
            transition_start: None,
            is_next: true,
            cached_hud: None,
            last_hud_state: None,
            last_minimized: false,
            last_frame_time: Instant::now(),
            ipc_rx,
            animation_cache: std::collections::HashMap::new(),
            tile_manager: None,
        };

        // Restore last session state
        if let Some(dir) = app.settings.last_image_dir.clone() {
            app.load_directory(dir);
        }
        if app.settings.play_music {
            app.restart_audio_if_enabled();
        }

        app
    }

    // ------------------------------------------------------------------
    // Persistent Storage
    // ------------------------------------------------------------------

    fn queue_save(&self) {
        let _ = self.save_tx.send(self.settings.clone());
    }

    // ------------------------------------------------------------------
    // Directory loading
    // ------------------------------------------------------------------

    fn open_directory_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(ref dir) = self.settings.last_image_dir.clone() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.load_directory(dir);
        }
    }

    fn load_directory(&mut self, dir: PathBuf) {
        self.settings.last_image_dir = Some(dir.clone());
        self.image_files.clear();
        self.current_index = 0;
        self.texture_cache.clear();
        self.animation_cache.clear();
        self.animation = None;
        self.prev_texture = None;
        self.transition_start = None;
        self.tile_manager = None;
        self.loader.cancel_all();
        self.pan_offset = Vec2::ZERO;
        self.error_message = None;
        self.scanning = true;
        self.status_message = format!(
            "Scanning {}…",
            dir.file_name().unwrap_or_default().to_string_lossy()
        );

        let (tx, rx) = crossbeam_channel::unbounded();
        self.scan_rx = Some(rx);
        scanner::scan_directory(dir, self.settings.recursive, tx);
    }

    // ------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------

    fn navigate_to(&mut self, new_index: usize) {
        if self.image_files.is_empty() {
            return;
        }
        
        let target_index = new_index % self.image_files.len();
        if target_index == self.current_index {
            return;
        }

        // Setup transition if enabled
        if self.settings.transition_style != TransitionStyle::None {
            if let Some(tex) = self.texture_cache.get(self.current_index) {
                self.prev_texture = Some(tex.clone());
                self.transition_start = Some(Instant::now());
                // Handle wrap-around logic for direction
                self.is_next = target_index > self.current_index || (target_index == 0 && self.current_index == self.image_files.len() - 1);
            }
        }

        self.current_index = target_index;
        self.zoom_factor = 1.0;
        self.pan_offset = Vec2::ZERO;
        // Reset animation playback — will be re-created if the new image is animated
        self.animation = None;
        // Clear tiled rendering state when switching images
        self.tile_manager = None;

        // Update resolution if already in cache
        if let Some(texture) = self.texture_cache.get(self.current_index) {
            let size = texture.size();
            self.current_image_res = Some((size[0] as u32, size[1] as u32));
        } else {
            self.current_image_res = None;
        }

        self.last_switch_time = Instant::now();
        self.error_message = None;
        self.cached_exif_text = None;

        // Try to pull from predictive cache if available
        if let Some(cached_anim) = self.animation_cache.get(&self.current_index) {
            self.animation = Some(AnimationPlayback {
                image_index: cached_anim.image_index,
                textures: cached_anim.textures.clone(),
                delays: cached_anim.delays.clone(),
                current_frame: 0,
                frame_start: Instant::now(),
            });
        }

        self.generation = self.generation.wrapping_add(1);
        self.loader.request_load(
            self.current_index,
            self.generation,
            self.image_files[self.current_index].clone(),
        );
        self.schedule_preloads(true);
    }

    fn delete_current_image(&mut self, permanent: bool) {
        if self.image_files.is_empty() {
            return;
        }

        let path_to_delete = self.image_files[self.current_index].clone();
        
        // Final sanity check: make sure file still exists
        if !path_to_delete.exists() {
            // Just remove from list if it's already gone
            self.image_files.remove(self.current_index);
        } else {
            let result = if permanent {
                std::fs::remove_file(&path_to_delete).map_err(|e| e.to_string())
            } else {
                trash::delete(&path_to_delete).map_err(|e| e.to_string())
            };

            if let Err(e) = result {
                self.error_message = Some(format!("Failed to delete file: {}", e));
                return;
            }
            
            // Successfully deleted
            self.image_files.remove(self.current_index);
        }

        // Texture cache is keyed by path/hash in our implementation usually, 
        // but if it's indexed, we need to clear or shift. 
        // Our texture_cache is likely a wrapper around a Map or similar.
        // Let's clear the entire cache to be safe or re-request.
        self.texture_cache.clear();
        self.animation_cache.clear();
        self.tile_manager = None;

        if self.image_files.is_empty() {
            self.current_index = 0;
            self.status_message = "No images left in directory.".to_string();
            self.current_image_res = None;
            self.animation = None;
            self.prev_texture = None;
            self.transition_start = None;
            self.cached_exif_text = None;
        } else {
            // Adjust current_index if we were at the last element
            if self.current_index >= self.image_files.len() {
                self.current_index = self.image_files.len() - 1;
            }
            
            // Reset state for new image
            self.animation = None;
            self.prev_texture = None;
            self.transition_start = None;
            self.zoom_factor = 1.0;
            self.pan_offset = Vec2::ZERO;
            self.cached_exif_text = None;
            self.error_message = None;

            // Load the image now at the current index
            self.generation = self.generation.wrapping_add(1);
            self.loader.request_load(
                self.current_index,
                self.generation,
                self.image_files[self.current_index].clone(),
            );
            self.schedule_preloads(true);
        }
        
        // Force HUD update
        self.last_hud_state = None;
    }

    fn navigate_next(&mut self) {
        if self.image_files.is_empty() {
            return;
        }
        let idx = (self.current_index + 1) % self.image_files.len();
        self.navigate_to(idx);
    }

    fn navigate_prev(&mut self) {
        if self.image_files.is_empty() {
            return;
        }
        let idx = if self.current_index == 0 {
            self.image_files.len() - 1
        } else {
            self.current_index - 1
        };
        self.navigate_to(idx);
    }

    fn navigate_first(&mut self) {
        self.navigate_to(0);
    }

    fn navigate_last(&mut self) {
        if !self.image_files.is_empty() {
            let last = self.image_files.len() - 1;
            self.navigate_to(last);
        }
    }

    // ------------------------------------------------------------------
    // Preloading
    // ------------------------------------------------------------------

    fn schedule_preloads(&mut self, forward: bool) {
        let n = self.image_files.len();
        if n == 0 {
            return;
        }
        let cur = self.current_index;
        let mut indices = vec![cur];
 
        if self.settings.preload {
            if forward {
                for i in 1..=PRELOAD_AHEAD {
                    indices.push((cur + i) % n);
                }
                if PRELOAD_BEHIND > 0 && cur > 0 {
                    indices.push(cur - 1);
                }
            } else {
                for i in 1..=PRELOAD_AHEAD {
                    if cur >= i {
                        indices.push(cur - i);
                    }
                }
                indices.push((cur + 1) % n);
            }
        }

        for idx in indices {
            if !self.texture_cache.contains(idx) && !self.loader.is_loading(idx) {
                let path = self.image_files[idx].clone();
                self.loader.request_load(idx, self.generation, path);
            }
        }
    }

    // ------------------------------------------------------------------
    // Background result processing
    // ------------------------------------------------------------------

    fn process_scan_results(&mut self) {
        let result = self.scan_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(files) = result {
            self.scan_rx = None;
            self.scanning = false;
            let count = files.len();
            self.image_files = files;
            self.current_index = 0;

            if let Some(ref path) = self.initial_image {
                // Fast path: try direct path comparison first (no syscalls)
                let found = self.image_files.iter().position(|p| p == path);
                let found = found.or_else(|| {
                    // Slow path: canonicalize and compare (syscalls, but only as fallback)
                    let target = path.canonicalize().unwrap_or_else(|_| path.clone());
                    self.image_files.iter().position(|p| {
                        p.canonicalize().unwrap_or_else(|_| p.clone()) == target
                    })
                });
                if let Some(pos) = found {
                    self.current_index = pos;
                }
                self.initial_image = None;
            } else if self.settings.resume_last_image {
                if let Some(last_path) = &self.settings.last_viewed_image {
                    if let Some(pos) = self.image_files.iter().position(|p| p == last_path) {
                        self.current_index = (pos + 1) % count;
                    }
                }
            }

            if count > 0 {
                self.status_message = format!("Found {count} images — use arrow keys to navigate");
                self.show_settings = false;
                self.schedule_preloads(true);
            } else {
                self.status_message = "No supported images found in this directory.".to_string();
            }
        }
    }

    fn process_loaded_images(&mut self, ctx: &Context) {
        while let Some(load_result) = self.loader.poll() {
            if load_result.generation != self.generation {
                continue;
            }
            let idx = load_result.index;
            match load_result.result {
                Ok(ImageData::Static(decoded)) => {
                    let color_image = ColorImage::from_rgba_unmultiplied(
                        [decoded.width as usize, decoded.height as usize],
                        &decoded.pixels,
                    );
                    let name = format!("img_{}", idx);
                    let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                    if let Some(evicted_idx) = self.texture_cache.insert(idx, handle, self.current_index) {
                        self.animation_cache.remove(&evicted_idx);
                    }
                    if idx == self.current_index {
                        self.current_image_res = Some((decoded.width, decoded.height));
                        if self.animation.as_ref().is_some_and(|a| a.image_index == idx) {
                            self.animation = None;
                        }
                    }
                }
                Ok(ImageData::LargeStatic(decoded)) => {
                    // Large image: create TileManager + preview, skip full GPU upload
                    if idx == self.current_index {
                        self.current_image_res = Some((decoded.width, decoded.height));
                        let screen_size = ctx.screen_rect().size();
                        let max_w = screen_size.x.max(1920.0) as u32;
                        let max_h = screen_size.y.max(1080.0) as u32;
                        let mut tm = TileManager::new(decoded.width, decoded.height, decoded.pixels);
                        let (pw, ph, preview_pixels) = tm.generate_preview(max_w, max_h);
                        let preview_img = ColorImage::from_rgba_unmultiplied(
                            [pw as usize, ph as usize],
                            &preview_pixels,
                        );
                        let preview_handle = ctx.load_texture(
                            format!("preview_{}", idx),
                            preview_img,
                            TextureOptions::LINEAR,
                        );
                        tm.preview_texture = Some(preview_handle);
                        self.tile_manager = Some(tm);
                        self.animation = None;
                        log::info!(
                            "Large image detected: {}x{} ({:.1} MP) — tiled mode active",
                            decoded.width, decoded.height,
                            (decoded.width as f64 * decoded.height as f64) / 1_000_000.0
                        );
                    }
                    // For non-current large images, we just drop the data (no preloading for large images)
                }
                Ok(ImageData::Animated(frames)) => {
                    // 1. Upload first frame to the main texture cache (for transitions/preview)
                    if let Some(first) = frames.first() {
                        let color_image = ColorImage::from_rgba_unmultiplied(
                            [first.width as usize, first.height as usize],
                            &first.pixels,
                        );
                        let name = format!("img_{}", idx);
                        let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                        if let Some(evicted_idx) = self.texture_cache.insert(idx, handle, self.current_index) {
                            self.animation_cache.remove(&evicted_idx);
                        }
                        if idx == self.current_index {
                            self.current_image_res = Some((first.width, first.height));
                        }
                    }

                    // 2. Predictive full-clip pre-upload
                    // We upload the full sequence if it's the current image OR within preload range
                    let cur = self.current_index;
                    let n = self.image_files.len();
                    let is_in_range = if n > 0 {
                        idx == cur 
                        || idx == (cur + 1) % n 
                        || (cur > 0 && idx == cur - 1) 
                        || (cur == 0 && idx == n - 1)
                    } else { false };

                    if is_in_range {
                        let mut textures = Vec::with_capacity(frames.len());
                        let mut delays = Vec::with_capacity(frames.len());
                        for (i, frame) in frames.iter().enumerate() {
                            let color_image = ColorImage::from_rgba_unmultiplied(
                                [frame.width as usize, frame.height as usize],
                                &frame.pixels,
                            );
                            let name = format!("anim_{}_{}", idx, i);
                            let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                            textures.push(handle);
                            delays.push(frame.delay);
                        }
                        
                        let playback = AnimationPlayback {
                            image_index: idx,
                            textures,
                            delays,
                            current_frame: 0,
                            frame_start: Instant::now(),
                        };

                        if idx == self.current_index {
                            self.animation = Some(AnimationPlayback {
                                image_index: playback.image_index,
                                textures: playback.textures.clone(),
                                delays: playback.delays.clone(),
                                current_frame: 0,
                                frame_start: Instant::now(),
                            });
                        }
                        self.animation_cache.insert(idx, playback);
                    }
                }
                Err(e) => {
                    log::warn!(
                        "Failed to load image at index {}: {e}",
                        idx
                    );
                    if idx == self.current_index {
                        self.error_message =
                            Some(format!("Failed to load image: {e}"));
                    }
                }
            }
        }
    }

    fn process_music_scan_results(&mut self) {
        if let Some(ref rx) = self.music_scan_rx {
            if let Ok(files) = rx.try_recv() {
                self.scanning_music = false;
                self.music_scan_rx = None;
                self.music_scan_cancel = None; // Thread finished or aborted
                
                // If it was aborted (returned empty), don't update count unless it's genuinely empty
                if !files.is_empty() {
                    self.cached_music_count = Some(files.len());
                    self.audio.start(files);
                    self.audio.set_volume(self.settings.volume);
                } else if self.music_scan_path.is_some() {
                    // Check if truly empty or just aborted
                    // Actually, if it's aborted, files will be empty. 
                    // We don't want to set cached_music_count to Some(0) if it was an abort.
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Keyboard input
    // ------------------------------------------------------------------

    fn handle_keyboard(&mut self, ctx: &Context) {
        // Collect flags to avoid borrow issues
        let mut nav_next = false;
        let mut nav_prev = false;
        let mut nav_first = false;
        let mut nav_last = false;
        let mut toggle_settings = false;
        let mut zoom_in = false;
        let mut zoom_out = false;
        let mut zoom_reset = false;
        let mut toggle_fullscreen = false;
        let mut toggle_scale_mode = false;
        let mut scroll_delta = 0.0_f32;
        let mut toggle_auto_switch = false;
        let mut toggle_goto = false;
        let mut do_refresh = false;
        #[allow(unused_mut)]
        let mut do_quit = false;
        let mut do_delete = false;
        let mut do_permanent_delete = false;

        // Collect all modal flags to prevent deletion when a dialog is active
        let any_modal_open = self.show_settings 
            || self.show_wallpaper_dialog 
            || self.show_goto 
            || self.show_exif_window;

        #[cfg(target_os = "windows")]
        let any_modal_open = any_modal_open || self.show_file_assoc_dialog;

        ctx.input(|i| {
            if i.key_pressed(Key::F5) {
                do_refresh = true;
            }
            if i.key_pressed(Key::Space) {
                toggle_auto_switch = true;
            }
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowDown) {
                nav_next = true;
            }
            if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowUp) {
                nav_prev = true;
            }
            if i.key_pressed(Key::Home) {
                nav_first = true;
            }
            if i.key_pressed(Key::End) {
                nav_last = true;
            }
            if i.key_pressed(Key::Escape) || i.key_pressed(Key::F1) {
                toggle_settings = true;
            }
            // Zoom keyboard: + / -
            if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
                zoom_in = true;
            }
            if i.key_pressed(Key::Minus) {
                zoom_out = true;
            }
            // '*' reset zoom: catches Shift+8 (main keyboard) AND Numpad*
            for ev in &i.events {
                if let egui::Event::Text(text) = ev {
                    if text == "*" {
                        zoom_reset = true;
                    }
                }
            }
            // Mouse wheel zoom — collected here, guarded before application below
            scroll_delta = i.smooth_scroll_delta.y;
            // F11 — toggle fullscreen
            if i.key_pressed(Key::F11) {
                toggle_fullscreen = true;
            }
            // Z — toggle scale mode (Fit ↔ Original)
            if i.key_pressed(Key::Z) {
                toggle_scale_mode = true;
            }
            // G / Ctrl+G — goto image by index
            if i.key_pressed(Key::G) {
                toggle_goto = true;
            }
            // Delete / Shift+Delete (Main window only)
            if !any_modal_open {
                if i.key_pressed(Key::Delete) {
                    if i.modifiers.shift {
                        do_permanent_delete = true;
                    } else {
                        do_delete = true;
                    }
                }
            }
            // Quit shortcut: Cmd+Q on macOS, Ctrl+Q on Linux.
            // On Windows, Alt+F4 is standard and is handled by the OS — no code needed.
            #[cfg(not(target_os = "windows"))]
            if i.modifiers.command && i.key_pressed(Key::Q) {
                do_quit = true;
            }
        });

        if do_delete { self.delete_current_image(false); }
        if do_permanent_delete { self.delete_current_image(true); }

        if do_refresh { self.load_directory(self.settings.last_image_dir.clone().unwrap_or_default()); }
        if nav_next { self.navigate_next(); }
        if nav_prev { self.navigate_prev(); }
        if nav_first { self.navigate_first(); }
        if nav_last { self.navigate_last(); }
        if toggle_settings {
            #[cfg(target_os = "windows")]
            if self.show_file_assoc_dialog {
                self.show_file_assoc_dialog = false;
            } else {
                self.show_settings = !self.show_settings;
            }
            #[cfg(not(target_os = "windows"))]
            {
                self.show_settings = !self.show_settings;
            }
        }
        if zoom_in {
            self.zoom_factor = (self.zoom_factor * 1.25).min(20.0);
        }
        if zoom_out {
            self.zoom_factor = (self.zoom_factor / 1.25).max(0.05);
        }
        if zoom_reset {
            self.zoom_factor = 1.0;
            self.pan_offset = Vec2::ZERO;
        }
        let ui_consuming_scroll = self.show_settings || ctx.wants_pointer_input();
        if !ui_consuming_scroll && scroll_delta > 0.0 {
            self.zoom_factor = (self.zoom_factor * 1.25).min(20.0);
        } else if !ui_consuming_scroll && scroll_delta < 0.0 {
            self.zoom_factor = (self.zoom_factor / 1.25).max(0.05);
        }
        if toggle_fullscreen {
            self.settings.fullscreen = !self.settings.fullscreen;
            self.pending_fullscreen = Some(self.settings.fullscreen);
            self.queue_save();
        }
        if toggle_scale_mode {
            self.settings.scale_mode = self.settings.scale_mode.toggled();
            self.zoom_factor = 1.0;
            self.pan_offset = Vec2::ZERO;
            self.queue_save();
        }
        if toggle_auto_switch && !self.show_settings && self.settings.auto_switch {
            self.slideshow_paused = !self.slideshow_paused;
            if !self.slideshow_paused {
                self.last_switch_time = Instant::now();
            }
        }
        if toggle_goto && !self.image_files.is_empty() {
            self.show_goto = !self.show_goto;
            if self.show_goto {
                self.goto_input.clear();
                self.goto_needs_focus = true;
            }
        }
        if do_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // ------------------------------------------------------------------
    // Auto-switch
    // ------------------------------------------------------------------

    fn check_auto_switch(&mut self) {
        if !self.settings.auto_switch || self.slideshow_paused || self.image_files.is_empty() {
            return;
        }
        let interval = Duration::from_secs_f32(self.settings.auto_switch_interval);
        if self.last_switch_time.elapsed() >= interval {
            let last = self.image_files.len() - 1;
            if !self.settings.loop_playback && self.current_index >= last {
                // Loop disabled: stop auto-switch at the last image
                return;
            }
            self.navigate_next();
        }
    }

    // ------------------------------------------------------------------
    // Audio helpers
    // ------------------------------------------------------------------

    fn open_music_file_dialog(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("Music files", &["mp3", "flac", "ogg", "wav", "aac", "m4a"]);
        if let Some(path) = dialog.pick_file() {
            self.settings.music_path = Some(path.clone());
            self.restart_audio_if_enabled();
        }
    }

    fn open_music_dir_dialog(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.settings.music_path = Some(dir.clone());
            self.restart_audio_if_enabled();
        }
    }

    fn restart_audio_if_enabled(&mut self) {
        // If not playing music, cancel any running scan and stop audio
        if !self.settings.play_music {
            if let Some(cancel) = self.music_scan_cancel.take() {
                cancel.store(false, Ordering::Relaxed);
            }
            self.audio.stop();
            self.scanning_music = false;
            self.music_scan_rx = None;
            self.music_scan_path = None;
            return;
        }

        // We ARE playing music.
        if let Some(path) = self.settings.music_path.clone() {
            // If already scanning or loaded THIS path, don't restart scan
            if self.music_scan_path.as_ref() == Some(&path) && (self.scanning_music || self.cached_music_count.is_some()) {
                return;
            }

            // Path changed or first scan: Cancel old scan if any
            if let Some(cancel) = self.music_scan_cancel.take() {
                cancel.store(false, Ordering::Relaxed);
            }
            self.audio.stop();

            self.scanning_music = true;
            self.music_scan_path = Some(path.clone());
            let cancel_signal = Arc::new(AtomicBool::new(true));
            self.music_scan_cancel = Some(Arc::clone(&cancel_signal));

            let (tx, rx) = crossbeam_channel::unbounded();
            self.music_scan_rx = Some(rx);

            // Background scan – do NOT block the UI
            std::thread::spawn(move || {
                let files = collect_music_files(&path, Some(cancel_signal));
                let _ = tx.send(files);
            });
        } else {
            // No path selected
            self.audio.stop();
            self.cached_music_count = None;
            self.music_scan_path = None;
        }
    }

    // ------------------------------------------------------------------
    // UI: Settings panel
    // ------------------------------------------------------------------

    fn draw_settings_panel(&mut self, ctx: &Context) {
        let mut open_dir = false;
        let mut open_music_file = false;
        let mut open_music_dir = false;
        let mut fullscreen_changed = false;
        let mut music_enabled_changed = false;
        let mut do_quit = false;

        egui::Window::new("⚙  Settings")
            .default_pos(Pos2::new(12.0, 12.0))
            .resizable(true)
            .collapsible(true)
            .frame(
                Frame::window(&ctx.global_style())
                    .fill(PANEL_BG)
                    .shadow(egui::epaint::Shadow::NONE),
            )
            .min_width(550.0)
            .default_width(640.0)
            .max_width(800.0)
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(Color32::WHITE);

                ui.heading(
                    RichText::new("🖼  Simple Image Viewer")
                        .color(ACCENT2)
                        .size(18.0),
                );
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(6.0);

                ui.columns(2, |cols| {
                cols[0].vertical(|ui| {
                
                // ── Directory ──────────────────────────────────────────────
                ui.label(RichText::new("Directory").color(ACCENT2).strong());
                ui.add_space(2.0);

                // Path display: short name in box, full path as tooltip
                let dir_full = self.settings.last_image_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned());
                let dir_short = self.settings.last_image_dir
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dir_full.clone().unwrap_or_default());
                let dir_empty = self.settings.last_image_dir.is_none();
                let dir_label = if dir_empty { "No directory selected".to_string() } else { dir_short };
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if styled_button(ui, "📁 Pick").clicked() {
                            open_dir = true;
                        }
                        ui.add_space(4.0);
                        if styled_button(ui, "🔄 Refresh").clicked() {
                            if let Some(dir) = self.settings.last_image_dir.clone() {
                                self.load_directory(dir);
                            }
                        }
                        
                        let box_w = (ui.available_width() - 16.0).max(20.0);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let resp = path_display_box(ui, &dir_label, dir_empty, box_w);
                            if let Some(full) = &dir_full {
                                resp.on_hover_text(full.as_str());
                            }
                        });
                    });
                });

                ui.add_space(4.0);
                let old_recursive = self.settings.recursive;
                ui.checkbox(&mut self.settings.recursive, "Recursive scan");
                if !old_recursive && self.settings.recursive {
                    // User just turned ON recursive scan — warn them first
                    let confirmed = rfd::MessageDialog::new()
                        .set_title("Enable Recursive Scan?")
                        .set_description(
                            "Recursive scan will search all subdirectories.\n\
                             This can take a very long time on large directory trees\n\
                             (e.g. a disk root). Are you sure?"
                        )
                        .set_buttons(rfd::MessageButtons::OkCancel)
                        .set_level(rfd::MessageLevel::Warning)
                        .show() == rfd::MessageDialogResult::Ok;
                    if !confirmed {
                        // User cancelled — revert the checkbox
                        self.settings.recursive = false;
                    }
                }
                if old_recursive != self.settings.recursive {
                    if let Some(dir) = self.settings.last_image_dir.clone() {
                        self.load_directory(dir);
                    }
                    self.queue_save();
                }

                if ui.checkbox(&mut self.settings.preload, "Enable image preloading").changed() {
                    self.queue_save();
                }

                if ui.checkbox(&mut self.settings.resume_last_image, "Resume from last viewed image").changed() {
                    self.queue_save();
                }

                if self.scanning {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(&self.status_message).color(TEXT_MUTED));
                    });
                }

                ui.add_space(8.0);

                // ── Display ────────────────────────────────────────────────
                ui.label(RichText::new("Display").color(ACCENT2).strong());
                ui.add_space(2.0);

                let old_fullscreen = self.settings.fullscreen;
                ui.checkbox(&mut self.settings.fullscreen, "Fullscreen (covers taskbar)");
                if old_fullscreen != self.settings.fullscreen {
                    fullscreen_changed = true;
                }

                ui.add_space(6.0);

                // Scale mode selector
                ui.label(RichText::new("Scale Mode").color(TEXT_MUTED).small());
                ui.add_space(2.0);
                let old_scale = self.settings.scale_mode;
                ui.horizontal(|ui| {
                    let fit_active = self.settings.scale_mode == ScaleMode::FitToWindow;
                    if ui.add(egui::Button::selectable(fit_active, "⛶  Fit to Window")).clicked()
                        && !fit_active
                    {
                        self.settings.scale_mode = ScaleMode::FitToWindow;
                    }
                    let orig_active = self.settings.scale_mode == ScaleMode::OriginalSize;
                    if ui.add(egui::Button::selectable(orig_active, "⊞  Original Size")).clicked()
                        && !orig_active
                    {
                        self.settings.scale_mode = ScaleMode::OriginalSize;
                    }
                });
                if old_scale != self.settings.scale_mode {
                    self.zoom_factor = 1.0;
                    self.pan_offset = Vec2::ZERO;
                    self.queue_save();
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Press Z to toggle scale mode")
                        .color(TEXT_MUTED)
                        .small(),
                );

                ui.add_space(6.0);
                ui.checkbox(&mut self.settings.show_osd, "Show OSD (filename, etc.)");
                
                // ── Transitions ──────────────────────────────────────────
                ui.add_space(8.0);
                ui.label(RichText::new("Image Transitions").color(ACCENT2).strong());
                ui.add_space(2.0);

                ui.horizontal(|ui| {
                    ui.label("Style:");
                    let old_style = self.settings.transition_style;
                    egui::ComboBox::from_id_salt("transition_style")
                        .selected_text(self.settings.transition_style.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.settings.transition_style, TransitionStyle::None, TransitionStyle::None.label());
                            ui.selectable_value(&mut self.settings.transition_style, TransitionStyle::Fade, TransitionStyle::Fade.label());
                            ui.selectable_value(&mut self.settings.transition_style, TransitionStyle::ZoomFade, TransitionStyle::ZoomFade.label());
                            ui.selectable_value(&mut self.settings.transition_style, TransitionStyle::Slide, TransitionStyle::Slide.label());
                            ui.selectable_value(&mut self.settings.transition_style, TransitionStyle::Push, TransitionStyle::Push.label());
                            ui.selectable_value(&mut self.settings.transition_style, TransitionStyle::PageFlip, TransitionStyle::PageFlip.label());
                            ui.selectable_value(&mut self.settings.transition_style, TransitionStyle::Ripple, TransitionStyle::Ripple.label());
                            ui.selectable_value(&mut self.settings.transition_style, TransitionStyle::Curtain, TransitionStyle::Curtain.label());
                        });
                    if old_style != self.settings.transition_style {
                        self.queue_save();
                    }
                });

                if self.settings.transition_style != TransitionStyle::None {
                    ui.horizontal(|ui| {
                        ui.label("Duration:");
                        let old_ms = self.settings.transition_ms;
                        ui.add(egui::Slider::new(&mut self.settings.transition_ms, 50..=2000).suffix("ms"));
                        if old_ms != self.settings.transition_ms {
                            self.queue_save();
                        }
                    });
                }
                
                }); // End Left Column
                
                cols[1].vertical(|ui| {
                // ── Slideshow ────────────────────────────────────────────
                ui.label(RichText::new("Slideshow").color(ACCENT2).strong());
                ui.add_space(2.0);

                let old_auto_switch = self.settings.auto_switch;
                if ui.checkbox(&mut self.settings.auto_switch, "Auto-advance to next picture").changed() {
                    self.slideshow_paused = false;  // Reset pause when toggling via UI
                }
                if self.settings.auto_switch {
                    ui.horizontal(|ui| {
                        ui.label("Interval (sec):");
                        ui.add(
                            egui::DragValue::new(&mut self.settings.auto_switch_interval)
                                .range(0.5..=3600.0)
                                .speed(0.5),
                        );
                    });
                    ui.checkbox(&mut self.settings.loop_playback, "Loop (wrap around to first image)");
                }
                if old_auto_switch != self.settings.auto_switch {
                    self.queue_save();
                }

                ui.add_space(8.0);

                // ── Music ──────────────────────────────────────────────────
                ui.label(RichText::new("Background Music").color(ACCENT2).strong());
                ui.add_space(2.0);

                let old_play_music = self.settings.play_music;
                ui.checkbox(&mut self.settings.play_music, "Play background music");
                ui.add_space(2.0);
                if old_play_music != self.settings.play_music {
                    music_enabled_changed = true;
                }

                if self.settings.play_music {
                    // Path: short name in box, full path as tooltip
                    let music_full = self.settings.music_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned());
                    let music_short = self.settings.music_path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| music_full.clone().unwrap_or_default());
                    let music_empty = self.settings.music_path.is_none();
                    let music_label = if music_empty { "No file or folder selected".to_string() } else { music_short };
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if styled_button(ui, "📂 Dir").clicked() {
                                open_music_dir = true;
                            }
                            if styled_button(ui, "🎵 File").clicked() {
                                open_music_file = true;
                            }
                            let box_w = (ui.available_width() - 16.0).max(20.0);
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                let resp = path_display_box(ui, &music_label, music_empty, box_w);
                                if let Some(full) = &music_full {
                                    resp.on_hover_text(full.as_str());
                                }
                            });
                        });
                    });
                    // File count badge
                    if self.settings.music_path.is_some() {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            if self.scanning_music {
                                ui.spinner();
                                ui.label(RichText::new("Scanning music…").color(TEXT_MUTED).small());
                            } else if let Some(count) = self.cached_music_count {
                                if count == 0 {
                                    ui.label(
                                        RichText::new("⚠ No supported audio files found")
                                            .color(Color32::from_rgb(255, 180, 60))
                                            .small(),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new(format!("♪ {count} file(s) ready"))
                                            .color(ACCENT2)
                                            .small(),
                                    );
                                }
                            }
                        });
                    }
                    // Now playing status
                    if let Some(track) = self.audio.get_current_track() {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("🎵 Now playing:").color(TEXT_MUTED).small());
                            ui.label(
                                RichText::new(track)
                                    .color(ACCENT2)
                                    .small()
                                    .italics(),
                            );
                        });
                    }

                    // Volume slider
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("🔊 Volume").color(TEXT_MUTED));
                        let old_vol = self.settings.volume;
                        let resp = ui.add(
                            egui::Slider::new(&mut self.settings.volume, 0.0..=1.0)
                                .show_value(true)
                                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                        );
                        // Update audio volume in real-time (cheap, no I/O)
                        if (old_vol - self.settings.volume).abs() > 0.001 {
                            self.audio.set_volume(self.settings.volume);
                        }
                        // Only persist to disk when user releases the slider
                        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                            self.queue_save();
                        }
                    });
                    // Audio error feedback
                    if let Some(err) = self.audio.take_error() {
                        ui.label(
                            RichText::new(format!("⚠ Audio: {err}"))
                                .color(Color32::from_rgb(255, 100, 100))
                                .small(),
                        );
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Font & Appearance ──────────────────────────────────────
                ui.label(RichText::new("Font & Appearance").color(ACCENT2).strong());
                ui.add_space(2.0);

                ui.horizontal(|ui| {
                    ui.label("Interface Size:");
                    let mut current_size = self.temp_font_size.unwrap_or(self.settings.font_size);
                    let resp = ui.add(egui::Slider::new(&mut current_size, 12.0..=32.0).step_by(1.0));
                    
                    if resp.dragged() {
                        self.temp_font_size = Some(current_size);
                    } else if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                        self.settings.font_size = current_size;
                        self.temp_font_size = None;
                        setup_visuals(ctx, &self.settings);
                        self.queue_save();
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Interface Font:");
                    let old_family = self.settings.font_family.clone();
                    egui::ComboBox::from_id_salt("font_family")
                        .selected_text(&self.settings.font_family)
                        .show_ui(ui, |ui| {
                            for family in &self.font_families {
                                ui.selectable_value(&mut self.settings.font_family, family.clone(), family);
                            }
                        });
                    if old_family != self.settings.font_family {
                        setup_fonts(ctx, &self.settings);
                        setup_visuals(ctx, &self.settings);
                        self.queue_save();
                    }
                });

                ui.add_space(8.0);
                }); // End of Right Column
                }); // End of ui.columns

                ui.separator();
                ui.add_space(6.0);

                // ── System Integration (Windows only) ─────────────────────
                #[cfg(target_os = "windows")]
                {
                    ui.label(RichText::new("System Integration (Windows)").color(ACCENT2).strong());
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Register this app in Windows \"Open With\" menu for image files.")
                            .color(TEXT_MUTED)
                            .small(),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if styled_button(ui, "📎 Associate Formats").clicked() {
                            // Reset all selections to true (default: all selected)
                            self.file_assoc_selections = vec![true; scanner::SUPPORTED_EXTENSIONS.len()];
                            self.show_file_assoc_dialog = true;
                        }
                        ui.add_space(8.0);
                        if styled_button(ui, "🗑 Remove Association").clicked() {
                            let confirmed = rfd::MessageDialog::new()
                                .set_title("Remove File Associations?")
                                .set_description(
                                    "This will remove all file associations created by this application \
                                     from the Windows registry.\n\n\
                                     The application will return to \"portable/green\" mode.\n\
                                     Other applications' associations will not be affected.\n\n\
                                     Continue?"
                                )
                                .set_buttons(rfd::MessageButtons::OkCancel)
                                .set_level(rfd::MessageLevel::Warning)
                                .show() == rfd::MessageDialogResult::Ok;
                            if confirmed {
                                crate::windows_utils::unregister_file_associations();
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                }

                // ── Exit area ────────────────────────────────────────────────
                #[cfg(target_os = "macos")]
                const QUIT_HINT: &str = "Esc / F1 to toggle this panel  │  Cmd+Q to quit";
                #[cfg(target_os = "linux")]
                const QUIT_HINT: &str = "Esc / F1 to toggle this panel  │  Ctrl+Q to quit";
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                const QUIT_HINT: &str = "Esc / F1 to toggle this panel  │  Alt+F4 to quit";

                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("✕  Exit Application").color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(180, 40, 40))
                            .corner_radius(egui::CornerRadius::same(4)),
                        )
                        .clicked()
                    {
                        do_quit = true;
                    }
                    ui.add_space(12.0);
                    ui.label(RichText::new(QUIT_HINT).color(TEXT_MUTED).small());
                });
            });

        // Deferred actions (avoid borrow issues with closures)
        if open_dir {
            self.open_directory_dialog();
            self.queue_save();
        }
        if open_music_file {
            self.open_music_file_dialog();
            self.queue_save();
        }
        if open_music_dir {
            self.open_music_dir_dialog();
            self.queue_save(); // saves music_path
        }
        if fullscreen_changed {
            self.pending_fullscreen = Some(self.settings.fullscreen);
            self.queue_save();
        }
        if music_enabled_changed {
            self.restart_audio_if_enabled();
            self.queue_save();
        }
        if do_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // ------------------------------------------------------------------
    // UI: Image canvas
    // ------------------------------------------------------------------

    fn draw_image_canvas_ui(&mut self, ui: &mut egui::Ui) {
        // Fill the area with dark background
        egui::Frame::NONE.fill(BG_DARK).show(ui, |ui| {
            let screen_rect = ui.max_rect();
            
            // Allocate the whole viewport for drag interaction and clicks early
            let canvas_resp = ui.allocate_rect(screen_rect, Sense::click_and_drag());
            
            if self.show_settings && canvas_resp.clicked() {
                self.show_settings = false;
            }

            if self.image_files.is_empty() {
                draw_empty_hint(ui, screen_rect);
                return;
            }

            // Error message
            if let Some(ref err) = self.error_message {
                ui.painter().text(
                    screen_rect.center(),
                    Align2::CENTER_CENTER,
                    format!("⚠ {err}"),
                    FontId::proportional(16.0),
                    Color32::from_rgb(255, 100, 100),
                );
                return;
            }

            // ── Tiled rendering path (large images) ──────────────────────
            if self.tile_manager.is_some() {
                if canvas_resp.dragged() {
                    self.pan_offset += canvas_resp.drag_delta();
                }

                // Extract immutable data first (avoids borrow conflict with compute_display_rect)
                let tm_ref = self.tile_manager.as_ref().unwrap();
                let img_size = Vec2::new(tm_ref.full_width as f32, tm_ref.full_height as f32);
                let full_w = tm_ref.full_width;
                let full_h = tm_ref.full_height;
                let dest = self.compute_display_rect(img_size, screen_rect);

                // 1. Draw preview texture as blurry background
                if let Some(ref preview) = self.tile_manager.as_ref().unwrap().preview_texture {
                    ui.painter().image(
                        preview.id(),
                        dest,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                // 2. Only render high-res tiles when zoomed in enough that they matter.
                //    At low zoom (fit-to-window), the preview texture is already sufficient.
                //    Threshold: 1 image pixel must map to at least 0.15 screen pixels.
                //    For a 30000px image on 1920px screen (fit=0.065), tiles appear at ~2.3x zoom.
                let effective_scale = dest.width() / img_size.x;
                if effective_scale >= 0.15 {
                    // Compute visible tiles (immutable borrow)
                    let visible = self.tile_manager.as_ref().unwrap().visible_tiles(dest, screen_rect);

                    // Upload and draw tiles (mutable borrow, scoped)
                    let ctx_ref = ui.ctx().clone();
                    const TILE_UPLOAD_QUOTA: usize = 4; // Max new tiles per frame
                    let mut newly_uploaded = 0;

                    {
                        let tm = self.tile_manager.as_mut().unwrap();
                        for (coord, tile_screen_rect, uv) in visible {
                            let allow_create = newly_uploaded < TILE_UPLOAD_QUOTA;
                            let (handle_opt, created) = tm.get_or_create_tile(coord, &ctx_ref, allow_create);
                            
                            if let Some(handle) = handle_opt {
                                if created {
                                    newly_uploaded += 1;
                                }
                                ui.painter().image(
                                    handle.id(),
                                    tile_screen_rect,
                                    uv,
                                    Color32::WHITE,
                                );
                            }
                            // If None, we don't draw anything (the blurry preview is already underneath)
                        }
                    }
                    
                    // If we didn't finish all tiles, request a repaint to catch them next frame
                    if newly_uploaded >= TILE_UPLOAD_QUOTA {
                        ui.ctx().request_repaint();
                    }
                }

                // HUD for tiled mode
                if self.settings.show_osd {
                    let zoom_pct = (self.zoom_factor * 100.0).round() as u32;
                    let hud_text = format!(
                        "[{}/{}]  {}x{}  {}%  TILED",
                        self.current_index + 1,
                        self.image_files.len(),
                        full_w,
                        full_h,
                        zoom_pct,
                    );
                    let galley = ui.painter().layout_no_wrap(
                        hud_text,
                        FontId::monospace(14.0),
                        Color32::WHITE,
                    );
                    let text_pos = Pos2::new(screen_rect.min.x + 12.0, screen_rect.max.y - 32.0);
                    let bg = Rect::from_min_size(
                        text_pos - Vec2::new(4.0, 2.0),
                        galley.size() + Vec2::new(8.0, 4.0),
                    );
                    ui.painter().rect_filled(bg, 4.0, Color32::from_black_alpha(160));
                    ui.painter().galley(text_pos, galley, Color32::WHITE);
                }

                // Context menu
                canvas_resp.context_menu(|ui| {
                    let path = &self.image_files[self.current_index];
                    let path_str = path.to_string_lossy().to_string();
                    if ui.button("📋 Copy Full Path").clicked() {
                        ui.ctx().copy_text(path_str);
                        ui.close();
                    }
                    if ui.button("ℹ View XMP Info").clicked() {
                        self.show_xmp_window = true;
                        ui.close();
                    }
                });

                return;
            }

            if let Some(texture) = self.texture_cache.get(self.current_index).cloned() {
                // For animated images, advance the frame and use the animation frame texture
                let texture = if let Some(ref mut anim) = self.animation {
                    if anim.image_index == self.current_index && !anim.textures.is_empty() {
                        // Advance frame if delay has elapsed
                        let elapsed = anim.frame_start.elapsed();
                        if elapsed >= anim.delays[anim.current_frame] {
                            anim.current_frame = (anim.current_frame + 1) % anim.textures.len();
                            anim.frame_start = Instant::now();
                        }
                        // Schedule repaint for next frame transition
                        let remaining = anim.delays[anim.current_frame]
                            .saturating_sub(anim.frame_start.elapsed());
                        ui.ctx().request_repaint_after(remaining);
                        anim.textures[anim.current_frame].clone()
                    } else {
                        texture
                    }
                } else {
                    texture
                };
                let img_size = texture.size_vec2();

                if canvas_resp.dragged() {
                    self.pan_offset += canvas_resp.drag_delta();
                }

                // Transition handling
                let mut alpha = 1.0;
                let mut scale = 1.0;
                let mut offset = Vec2::ZERO;
                let mut prev_alpha = 0.0;
                let mut prev_scale = 1.0;
                let mut prev_offset = Vec2::ZERO;
                let mut is_animating = false;

                if let Some(start) = self.transition_start {
                    let elapsed = start.elapsed().as_secs_f32();
                    let duration = self.settings.transition_ms as f32 / 1000.0;
                    if elapsed < duration {
                        is_animating = true;
                        let t = (elapsed / duration).clamp(0.0, 1.0);
                        // Easing: Cubic Out
                        let ease_out = 1.0 - (1.0 - t).powi(3);

                        match self.settings.transition_style {
                            TransitionStyle::Fade => {
                                alpha = ease_out;
                                prev_alpha = 1.0 - t;
                            }
                            TransitionStyle::ZoomFade => {
                                alpha = ease_out;
                                scale = 0.95 + 0.05 * ease_out;
                                prev_alpha = 1.0 - t;
                                prev_scale = 1.0 + 0.05 * t;
                            }
                            TransitionStyle::Slide => {
                                let dir = if self.is_next { 1.0 } else { -1.0 };
                                offset = Vec2::new(screen_rect.width() * dir * (1.0 - ease_out), 0.0);
                                prev_alpha = 1.0 - t;
                            }
                            TransitionStyle::Push => {
                                let dir = if self.is_next { 1.0 } else { -1.0 };
                                offset = Vec2::new(screen_rect.width() * dir * (1.0 - ease_out), 0.0);
                                prev_offset = Vec2::new(-screen_rect.width() * dir * ease_out, 0.0);
                                prev_alpha = 1.0;
                            }
                            TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain => {
                                // Custom rendering; keep is_animating true.
                            }
                            _ => { is_animating = false; }
                        }
                    } else {
                        self.transition_start = None;
                        self.prev_texture = None;
                    }
                }

                // Compute current display rect
                let dest = self.compute_display_rect(img_size, screen_rect);
                let final_dest = Rect::from_center_size(
                    dest.center() + offset,
                    dest.size() * scale
                );

                // DRAW SEQUENCE:
                // PageFlip and Ripple need custom rendering order.
                if is_animating && matches!(self.settings.transition_style, TransitionStyle::PageFlip | TransitionStyle::Ripple) {
                    match self.settings.transition_style {
                        TransitionStyle::PageFlip => {
                            if let Some(prev) = &self.prev_texture {
                                let p_size = prev.size_vec2();
                                let p_dest = self.compute_display_rect(p_size, screen_rect);
                                // The boundary of the animation is the union of the old and new image areas
                                let union_rect = p_dest.union(final_dest);

                                let elapsed = self.transition_start.unwrap().elapsed().as_secs_f32();
                                let duration = self.settings.transition_ms as f32 / 1000.0;
                                let t = (elapsed / duration).clamp(0.0, 1.0);
                                let ease_in_out = 3.0 * t * t - 2.0 * t * t * t;

                                let clip_x = if self.is_next {
                                    union_rect.max.x - (union_rect.width() * ease_in_out)
                                } else {
                                    union_rect.min.x + (union_rect.width() * ease_in_out)
                                };

                                // 1. Draw NEW image (revealed part, clipped)
                                let mut new_clip = union_rect;
                                if self.is_next {
                                    new_clip.min.x = clip_x;
                                } else {
                                    new_clip.max.x = clip_x;
                                }
                                ui.painter().with_clip_rect(new_clip).image(
                                    texture.id(),
                                    final_dest,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );

                                // 2. Draw OLD image (unrevealed part, clipped)
                                let mut old_clip = union_rect;
                                if self.is_next {
                                    old_clip.max.x = clip_x;
                                } else {
                                    old_clip.min.x = clip_x;
                                }

                                ui.painter().with_clip_rect(old_clip).image(
                                    prev.id(),
                                    p_dest,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );

                                // Page fold shadow (relative to union area height)
                                let shadow_width = 40.0;
                                let shadow_alpha = (1.0 - ease_in_out) * 0.4;
                                let shadow_rect = if self.is_next {
                                    Rect::from_min_max(
                                        Pos2::new(clip_x - shadow_width, union_rect.min.y),
                                        Pos2::new(clip_x, union_rect.max.y)
                                    )
                                } else {
                                    Rect::from_min_max(
                                        Pos2::new(clip_x, union_rect.min.y),
                                        Pos2::new(clip_x + shadow_width, union_rect.max.y)
                                    )
                                };

                                let color_shadow = Color32::from_black_alpha((shadow_alpha * 255.0) as u8);
                                let color_transparent = Color32::TRANSPARENT;
                                let mut mesh = egui::Mesh::default();
                                let (c_left, c_right) = if self.is_next {
                                    (color_transparent, color_shadow)
                                } else {
                                    (color_shadow, color_transparent)
                                };
                                mesh.colored_vertex(shadow_rect.left_top(), c_left);
                                mesh.colored_vertex(shadow_rect.right_top(), c_right);
                                mesh.colored_vertex(shadow_rect.right_bottom(), c_right);
                                mesh.colored_vertex(shadow_rect.left_bottom(), c_left);
                                mesh.add_triangle(0, 1, 2);
                                mesh.add_triangle(0, 2, 3);
                                ui.painter().add(egui::Shape::mesh(mesh));
                            }
                        }

                        TransitionStyle::Ripple => {
                            // 1. Draw OLD image as full background
                            if let Some(prev) = &self.prev_texture {
                                let p_size = prev.size_vec2();
                                let p_dest = self.compute_display_rect(p_size, screen_rect);
                                ui.painter().image(
                                    prev.id(),
                                    p_dest,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }

                            // 2. Compute ripple state
                            let elapsed = self.transition_start.unwrap().elapsed().as_secs_f32();
                            let duration = self.settings.transition_ms as f32 / 1000.0;
                            let t = (elapsed / duration).clamp(0.0, 1.0);
                            let ease = 3.0 * t * t - 2.0 * t * t * t; // smoothstep

                            let center = dest.center();
                            // Max radius: distance from image center to farthest screen corner
                            let corners = [
                                screen_rect.left_top(), screen_rect.right_top(),
                                screen_rect.left_bottom(), screen_rect.right_bottom(),
                            ];
                            let max_radius = corners.iter()
                                .map(|c| center.distance(*c))
                                .fold(0.0f32, f32::max);
                            let current_radius = max_radius * ease;

                            // 3. Create circular textured mesh for new image (triangle fan)
                            let segments = 128u32;
                            let mut mesh = egui::Mesh::default();
                            mesh.texture_id = texture.id();

                            // Center vertex
                            let center_uv = Pos2::new(
                                (center.x - dest.min.x) / dest.width(),
                                (center.y - dest.min.y) / dest.height(),
                            );
                            mesh.vertices.push(egui::epaint::Vertex {
                                pos: center,
                                uv: center_uv,
                                color: Color32::WHITE,
                            });

                            // Edge vertices around the circle
                            for i in 0..=segments {
                                let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
                                let pos = Pos2::new(
                                    center.x + current_radius * angle.cos(),
                                    center.y + current_radius * angle.sin(),
                                );
                                let uv = Pos2::new(
                                    (pos.x - dest.min.x) / dest.width(),
                                    (pos.y - dest.min.y) / dest.height(),
                                );
                                mesh.vertices.push(egui::epaint::Vertex {
                                    pos,
                                    uv,
                                    color: Color32::WHITE,
                                });
                            }

                            // Triangle fan indices
                            for i in 0..segments {
                                mesh.indices.push(0);       // center
                                mesh.indices.push(i + 1);
                                mesh.indices.push(i + 2);
                            }

                            ui.painter().with_clip_rect(dest).add(egui::Shape::mesh(mesh));

                            // 4. Draw water ripple rings at the expanding edge
                            for ring in 0..4u32 {
                                let ring_radius = current_radius - ring as f32 * 14.0;
                                if ring_radius <= 2.0 { continue; }
                                let ring_alpha = (0.35 - ring as f32 * 0.09).max(0.0);
                                let ring_color = Color32::from_rgba_unmultiplied(
                                    180, 215, 255,
                                    (ring_alpha * 255.0) as u8,
                                );
                                let ring_width = 2.5 - ring as f32 * 0.5;

                                let points: Vec<Pos2> = (0..=segments)
                                    .map(|i| {
                                        let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
                                        Pos2::new(
                                            center.x + ring_radius * angle.cos(),
                                            center.y + ring_radius * angle.sin(),
                                        )
                                    })
                                    .collect();
                                ui.painter().add(egui::Shape::line(
                                    points,
                                    egui::Stroke::new(ring_width, ring_color),
                                ));
                            }
                        }

                        _ => unreachable!(),
                    }
                    ui.ctx().request_repaint();

                } else if is_animating && self.settings.transition_style == TransitionStyle::Curtain {
                    if let Some(prev) = &self.prev_texture {
                        let p_size = prev.size_vec2();
                        let p_dest = self.compute_display_rect(p_size, screen_rect);
                        // Smart boundary: union of old and new image rects
                        let union_rect = p_dest.union(final_dest);

                        let elapsed = self.transition_start.unwrap().elapsed().as_secs_f32();
                        let duration = self.settings.transition_ms as f32 / 1000.0;
                        let t = (elapsed / duration).clamp(0.0, 1.0);
                        let ease = 1.0 - (1.0 - t).powi(3); // Cubic Out

                        let center_x = union_rect.center().x;
                        let half_w = union_rect.width() / 2.0;
                        let shift = ease * half_w;

                        // 1. Draw NEW image (revealed in the gap, clipped)
                        let new_clip = Rect::from_min_max(
                            Pos2::new(center_x - shift, union_rect.min.y),
                            Pos2::new(center_x + shift, union_rect.max.y),
                        );
                        ui.painter().with_clip_rect(new_clip).image(
                            texture.id(),
                            final_dest,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );

                        // 2. Draw OLD image as two sliding curtain halves
                        // Left curtain: slides left
                        let left_clip = Rect::from_min_max(
                            union_rect.left_top(),
                            Pos2::new(center_x - shift, union_rect.max.y),
                        );
                        let left_dest = p_dest.translate(Vec2::new(-shift, 0.0));
                        ui.painter().with_clip_rect(left_clip).image(
                            prev.id(),
                            left_dest,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );

                        // Right curtain: slides right
                        let right_clip = Rect::from_min_max(
                            Pos2::new(center_x + shift, union_rect.min.y),
                            union_rect.right_bottom(),
                        );
                        let right_dest = p_dest.translate(Vec2::new(shift, 0.0));
                        ui.painter().with_clip_rect(right_clip).image(
                            prev.id(),
                            right_dest,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );

                        // Shadow at the split edges
                        let shadow_w = 30.0;
                        let shadow_alpha = (1.0 - ease) * 0.45;
                        let shadow_color = Color32::from_black_alpha((shadow_alpha * 255.0) as u8);
                        let transparent = Color32::TRANSPARENT;

                        // Left curtain inner shadow (right edge)
                        let ls_rect = Rect::from_min_max(
                            Pos2::new(center_x - shift - shadow_w, union_rect.min.y),
                            Pos2::new(center_x - shift, union_rect.max.y),
                        );
                        let mut lm = egui::Mesh::default();
                        lm.colored_vertex(ls_rect.left_top(), transparent);
                        lm.colored_vertex(ls_rect.right_top(), shadow_color);
                        lm.colored_vertex(ls_rect.right_bottom(), shadow_color);
                        lm.colored_vertex(ls_rect.left_bottom(), transparent);
                        lm.add_triangle(0, 1, 2);
                        lm.add_triangle(0, 2, 3);
                        ui.painter().add(egui::Shape::mesh(lm));

                        // Right curtain inner shadow (left edge)
                        let rs_rect = Rect::from_min_max(
                            Pos2::new(center_x + shift, union_rect.min.y),
                            Pos2::new(center_x + shift + shadow_w, union_rect.max.y),
                        );
                        let mut rm = egui::Mesh::default();
                        rm.colored_vertex(rs_rect.left_top(), shadow_color);
                        rm.colored_vertex(rs_rect.right_top(), transparent);
                        rm.colored_vertex(rs_rect.right_bottom(), transparent);
                        rm.colored_vertex(rs_rect.left_bottom(), shadow_color);
                        rm.add_triangle(0, 1, 2);
                        rm.add_triangle(0, 2, 3);
                        ui.painter().add(egui::Shape::mesh(rm));
                    }
                    ui.ctx().request_repaint();

                } else {
                    // Standard Transitions (Fade/Slide/Push):
                    // 1. Draw OLD image (underneath or fading out)
                    if is_animating {
                        if let Some(prev) = &self.prev_texture {
                            let p_size = prev.size_vec2();
                            let p_dest = self.compute_display_rect(p_size, screen_rect);
                            let p_final_dest = Rect::from_center_size(
                                p_dest.center() + prev_offset,
                                p_dest.size() * prev_scale
                            );
                            ui.painter().image(
                                prev.id(),
                                p_final_dest,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE.linear_multiply(prev_alpha),
                            );
                        }
                        ui.ctx().request_repaint();
                    }

                    // 2. Draw NEW image (on top, with alpha/motion)
                    ui.painter().image(
                        texture.id(),
                        final_dest,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE.linear_multiply(alpha),
                    );
                }

                // Right-click context menu (defines interactions for the canvas area)
                canvas_resp.context_menu(|ui| {
                    let path = &self.image_files[self.current_index];
                    let path_str = path.to_string_lossy().to_string();

                    if ui.button("📋 Copy Full Path").clicked() {
                        ui.ctx().copy_text(path_str.clone());
                        ui.close();
                    }

                    if ui.button("📁 Copy File").clicked() {
                        copy_file_to_clipboard(&path_str);
                        ui.close();
                    }

                    ui.separator();

                    if ui.button("ℹ View EXIF Info").clicked() {
                        if let Some(text) = extract_exif(path) {
                            self.cached_exif_text = Some(text);
                        } else {
                            self.cached_exif_text = Some("No EXIF data found in this image.".to_string());
                        }
                        self.show_exif_window = true;
                        ui.close();
                    }

                    if ui.button("ℹ View XMP Info").clicked() {
                        self.show_xmp_window = true;
                        ui.close();
                    }
                    
                    ui.separator();
                    if ui.button("🖼 Set as desktop wallpaper…").clicked() {
                        self.show_wallpaper_dialog = true;
                        if let Ok(p) = wallpaper::get() {
                            self.current_system_wallpaper = Some(p);
                        } else {
                            self.current_system_wallpaper = Some("Unknown".to_string());
                        }
                        ui.close();
                    }
                });

                if self.settings.show_osd {
                    // HUD overlay
                    let zoom_pct = (self.zoom_factor * 100.0).round() as u32;
                    let img_w = img_size.x.round() as u32;
                    let img_h = img_size.y.round() as u32;
                    let mode_label = self.settings.scale_mode.label();

                    let current_state = HudState {
                        index: self.current_index,
                        total: self.image_files.len(),
                        zoom_pct,
                        res: (img_w, img_h),
                        mode: mode_label.to_string(),
                        current_track: self.audio.get_current_track(),
                    };

                    if self.last_hud_state.as_ref() != Some(&current_state) {
                        let fname = self.image_files[self.current_index]
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        
                        let mut hud = format!(
                            "{} / {}    {}    {}%    {}×{}    [{}]",
                            current_state.index + 1,
                            current_state.total,
                            fname,
                            current_state.zoom_pct,
                            current_state.res.0,
                            current_state.res.1,
                            current_state.mode,
                        );

                        // Add Music info if playing
                        if let Some(ref track) = current_state.current_track {
                            hud.push_str(&format!("    ♪ {}", track));
                        }
                        
                        self.cached_hud = Some(hud);
                        self.last_hud_state = Some(current_state);
                    }

                    if let Some(hud) = &self.cached_hud {
                        let hud_pos = screen_rect.left_bottom() + Vec2::new(12.0, -12.0);
                        ui.painter().text(
                            hud_pos,
                            Align2::LEFT_BOTTOM,
                            hud,
                            FontId::proportional(13.0),
                            Color32::from_rgba_unmultiplied(220, 220, 240, 210),
                        );
                    }

                    // Hint when settings hidden
                    if !self.show_settings {
                        ui.painter().text(
                            screen_rect.right_bottom() + Vec2::new(-12.0, -12.0),
                            Align2::RIGHT_BOTTOM,
                            "F1 — settings  │  +/- or scroll — zoom  │  * reset  │  Z — fit/original  │  G — goto  │  F11 — fullscreen",
                            FontId::proportional(11.0),
                            Color32::from_rgba_unmultiplied(160, 160, 180, 140),
                        );
                    }
                }
            } else {
                if self.settings.show_osd {
                    // Loading spinner
                    ui.painter().text(
                        screen_rect.center() - Vec2::new(0.0, 20.0),
                        Align2::CENTER_BOTTOM,
                        "Loading…",
                        FontId::proportional(16.0),
                        TEXT_MUTED,
                    );
                }
            }
        });
    }

    /// Compute the display rect for an image texture within the screen.
    fn compute_display_rect(&self, img_size: Vec2, screen_rect: Rect) -> Rect {
        match self.settings.scale_mode {
            ScaleMode::FitToWindow => {
                let fit_scale = (screen_rect.width() / img_size.x)
                    .min(screen_rect.height() / img_size.y);
                let scale = fit_scale * self.zoom_factor;
                let disp = img_size * scale;
                let off = (screen_rect.size() - disp) * 0.5;
                Rect::from_min_size(
                    screen_rect.min + off + self.pan_offset,
                    disp,
                )
            }
            ScaleMode::OriginalSize => {
                // Divide by pixels_per_point so 1 image pixel = 1 physical screen pixel
                // on HiDPI/Retina displays (e.g. 4K at 200% scaling).
                let ppp = self.cached_pixels_per_point;
                let disp = img_size * (self.zoom_factor / ppp);
                let center = screen_rect.center() + self.pan_offset;
                Rect::from_center_size(center, disp)
            }
        }
    }

    // ------------------------------------------------------------------
    // Wallpaper Dialog
    // ------------------------------------------------------------------

    fn draw_wallpaper_dialog(&mut self, ctx: &Context) {
        if !self.show_wallpaper_dialog {
            return;
        }

        let mut do_close = false;
        let mut do_set = false;

        egui::Window::new("Set as desktop wallpaper")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .resizable(false)
            .collapsible(false)
            .frame(
                Frame::window(&ctx.global_style())
                    .fill(PANEL_BG)
                    .shadow(egui::epaint::Shadow::NONE),
            )
            .fixed_size([520.0, 320.0])
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(Color32::WHITE);
                ui.add_space(8.0);

                if let Some(ref current) = self.current_system_wallpaper {
                    ui.label(RichText::new("Current System Wallpaper:").color(TEXT_MUTED).small());
                    egui::ScrollArea::horizontal()
                        .id_salt("curr_wp_scroll")
                        .min_scrolled_height(24.0)
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.add_space(2.0);
                                ui.add(egui::Label::new(current).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
                                ui.add_space(4.0);
                            });
                        });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);
                }

                let path = self.image_files[self.current_index].to_string_lossy().into_owned();
                ui.label(RichText::new("New Image Path:").color(TEXT_MUTED).small());
                egui::ScrollArea::horizontal()
                    .id_salt("new_wp_scroll")
                    .min_scrolled_height(24.0)
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.add(egui::Label::new(&path).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
                            ui.add_space(4.0);
                        });
                    });
                
                if let Some((w, h)) = self.current_image_res {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Original Resolution:").color(TEXT_MUTED).small());
                    ui.label(format!("{} × {} pixels", w, h));
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Wallpaper Mode:").color(ACCENT2).strong());

                ui.vertical(|ui| {
                    ui.radio_value(&mut self.selected_wallpaper_mode, "Crop".to_string(), "Crop (Fill)");
                    ui.radio_value(&mut self.selected_wallpaper_mode, "Fit".to_string(), "Fit");
                    ui.radio_value(&mut self.selected_wallpaper_mode, "Stretch".to_string(), "Stretch");
                    ui.radio_value(&mut self.selected_wallpaper_mode, "Tile".to_string(), "Tile");
                    ui.radio_value(&mut self.selected_wallpaper_mode, "Center".to_string(), "Center");
                    ui.radio_value(&mut self.selected_wallpaper_mode, "Span".to_string(), "Span (Multi-monitor)");
                });

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new(" Set Wallpaper ").color(Color32::WHITE)).clicked() {
                        do_set = true;
                    }
                    if ui.button(" Cancel ").clicked() {
                        do_close = true;
                    }
                });
            });

        if do_set {
            let path = self.image_files[self.current_index].clone();
            let mode_str = self.selected_wallpaper_mode.clone();
            
            // Map string to wallpaper::Mode
            let mode = match mode_str.as_str() {
                "Center" => wallpaper::Mode::Center,
                "Crop" => wallpaper::Mode::Crop,
                "Fit" => wallpaper::Mode::Fit,
                "Span" => wallpaper::Mode::Span,
                "Stretch" => wallpaper::Mode::Stretch,
                "Tile" => wallpaper::Mode::Tile,
                _ => wallpaper::Mode::Crop,
            };

            // execute wallpaper setting (can take a second on Windows)
            std::thread::spawn(move || {
                let _ = wallpaper::set_mode(mode);
                if let Err(e) = wallpaper::set_from_path(path.to_str().unwrap_or_default()) {
                    log::error!("Failed to set wallpaper: {e}");
                }
            });
            
            do_close = true;
        }

        if do_close {
            self.show_wallpaper_dialog = false;
        }
    }

    // ------------------------------------------------------------------
    // UI: Goto dialog
    // ------------------------------------------------------------------

    fn draw_goto_dialog(&mut self, ctx: &Context) {
        let total = self.image_files.len();
        if total == 0 {
            return;
        }

        let mut do_close = false;
        let mut do_jump = false;

        egui::Window::new("Go to image…")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .resizable(false)
            .collapsible(false)
            .frame(
                Frame::window(&ctx.global_style())
                    .fill(PANEL_BG)
                    .shadow(egui::epaint::Shadow::NONE),
            )
            .fixed_size([320.0, 120.0])
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(Color32::WHITE);
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Enter image number (1 – {})", total))
                        .color(TEXT_MUTED)
                        .small(),
                );
                ui.add_space(6.0);

                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.goto_input)
                        .desired_width(f32::INFINITY)
                        .hint_text(format!("{}", self.current_index + 1)),
                );

                // Auto-focus the text field when the dialog first opens
                if self.goto_needs_focus {
                    resp.request_focus();
                    self.goto_needs_focus = false;
                }

                // Enter key confirms; Escape closes
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    do_jump = true;
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    do_close = true;
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if styled_button(ui, "Go").clicked() {
                        do_jump = true;
                    }
                    if styled_button(ui, "Cancel").clicked() {
                        do_close = true;
                    }
                });
            });

        if do_jump {
            let raw: usize = self.goto_input.trim().parse().unwrap_or(0);
            // Input is 1-based; clamp to valid range
            if raw >= 1 {
                let idx = (raw - 1).min(total - 1);
                self.show_goto = false;
                self.navigate_to(idx);
            }
        }
        if do_close {
            self.show_goto = false;
        }
    }

    // ------------------------------------------------------------------
    // UI: File association dialog (Windows only)
    // ------------------------------------------------------------------

    #[cfg(target_os = "windows")]
    fn draw_file_assoc_dialog(&mut self, ctx: &Context) {
        if !self.show_file_assoc_dialog {
            return;
        }

        // Dark background overlay (purely visual, settings panel is hidden)
        let screen_rect = ctx.screen_rect();
        let bg_layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("file_assoc_bg"));
        ctx.layer_painter(bg_layer).add(
            egui::Shape::rect_filled(
                screen_rect,
                egui::CornerRadius::ZERO,
                Color32::from_black_alpha(180),
            ),
        );

        let mut do_apply = false;
        let mut do_cancel = false;

        egui::Window::new("📎  Select Image Formats to Associate")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_width(420.0)
            .frame(
                Frame::window(&ctx.global_style())
                    .fill(PANEL_BG)
                    .shadow(egui::epaint::Shadow::NONE),
            )
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(Color32::WHITE);

                ui.label(
                    RichText::new("Select which image formats this application\nshould be associated with in Windows:")
                        .color(TEXT_MUTED),
                );
                ui.add_space(8.0);

                // Select All / Deselect All
                ui.horizontal(|ui| {
                    if ui.button("✔ Select All").clicked() {
                        for sel in self.file_assoc_selections.iter_mut() {
                            *sel = true;
                        }
                    }
                    if ui.button("✘ Deselect All").clicked() {
                        for sel in self.file_assoc_selections.iter_mut() {
                            *sel = false;
                        }
                    }
                });
                ui.add_space(4.0);

                // Scrollable area with checkboxes in a 3-column grid
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    let exts = scanner::SUPPORTED_EXTENSIONS;
                    let cols = 3;
                    let rows = (exts.len() + cols - 1) / cols;

                    egui::Grid::new("file_assoc_grid")
                        .num_columns(cols)
                        .spacing([24.0, 4.0])
                        .show(ui, |ui| {
                            for row in 0..rows {
                                for col in 0..cols {
                                    let idx = row * cols + col;
                                    if idx < exts.len() {
                                        let label = format!(".{}", exts[idx]);
                                        ui.checkbox(&mut self.file_assoc_selections[idx], label);
                                    }
                                }
                                ui.end_row();
                            }
                        });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Count selected
                let selected_count = self.file_assoc_selections.iter().filter(|&&s| s).count();

                ui.horizontal(|ui| {
                    let apply_enabled = selected_count > 0;
                    if ui
                        .add_enabled(
                            apply_enabled,
                            egui::Button::new(
                                RichText::new(format!("✔  Apply ({} formats)", selected_count))
                                    .color(Color32::WHITE)
                            )
                            .fill(ACCENT)
                            .corner_radius(egui::CornerRadius::same(4)),
                        )
                        .clicked()
                    {
                        do_apply = true;
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new("✕  Cancel")
                                .corner_radius(egui::CornerRadius::same(4)),
                        )
                        .clicked()
                    {
                        do_cancel = true;
                    }
                });
            });

        if do_apply {
            // Collect selected extensions
            let selected: Vec<&str> = scanner::SUPPORTED_EXTENSIONS
                .iter()
                .zip(self.file_assoc_selections.iter())
                .filter(|(_, sel)| **sel)
                .map(|(&ext, _)| ext)
                .collect();
            crate::windows_utils::register_file_associations(&selected);
            self.show_file_assoc_dialog = false;

            rfd::MessageDialog::new()
                .set_title("File Association Registered")
                .set_description(
                    "File associations have been registered successfully!\n\n\
                     To set Simple Image Viewer as the default image viewer:\n\
                     1. Double-click any image file in Explorer\n\
                     2. Select \"Simple Image Viewer\" from the list\n\
                     3. Check \"Always use this app\"\n\n\
                     Usually you only need to do this once, and Windows will \
                     automatically apply it to all associated image types."
                )
                .set_buttons(rfd::MessageButtons::Ok)
                .set_level(rfd::MessageLevel::Info)
                .show();
        }
        if do_cancel {
            self.show_file_assoc_dialog = false;
        }
    }
}

impl eframe::App for ImageViewerApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.settings.resume_last_image && !self.image_files.is_empty() {
            self.settings.last_viewed_image = Some(self.image_files[self.current_index].clone());
            self.queue_save();
        }
    }

    /// Background logic: scanning, loading, auto-switch, keyboard, timers.
    /// Called before each ui() call (and also when hidden but repaint requested).
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Process IPC messages
        while let Ok(msg) = self.ipc_rx.try_recv() {
            match msg {
                IpcMessage::OpenImage(path) => {
                    log::info!("IPC: open image {:?}", path);
                    if let Some(parent) = path.parent() {
                        let same_dir = self.settings.last_image_dir
                            .as_ref()
                            .map(|d| d == &parent.to_path_buf())
                            .unwrap_or(false);

                        if same_dir && !self.image_files.is_empty() {
                            // Same directory: just find and jump to the target image
                            if let Some(pos) = self.image_files.iter().position(|p| p == &path) {
                                if self.settings.auto_switch {
                                    self.settings.auto_switch = false;
                                }
                                self.navigate_to(pos);
                            } else {
                                // File not in our list (maybe newly added) — full rescan
                                self.initial_image = Some(path.clone());
                                if self.settings.auto_switch {
                                    self.settings.auto_switch = false;
                                }
                                self.load_directory(parent.to_path_buf());
                            }
                        } else {
                            // Different directory — full scan
                            self.settings.last_image_dir = Some(parent.to_path_buf());
                            self.queue_save();
                            self.initial_image = Some(path.clone());
                            if self.settings.auto_switch {
                                self.settings.auto_switch = false;
                            }
                            self.load_directory(parent.to_path_buf());
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        crate::ipc::force_foreground();
                    }
                }
                IpcMessage::OpenImageNoRecursive(path) => {
                    log::info!("IPC: open image (no-recursive) {:?}", path);
                    if let Some(parent) = path.parent() {
                        let same_dir = self.settings.last_image_dir
                            .as_ref()
                            .map(|d| d == &parent.to_path_buf())
                            .unwrap_or(false);

                        if same_dir && !self.image_files.is_empty() {
                            // Same directory: just jump, no rescan needed
                            if let Some(pos) = self.image_files.iter().position(|p| p == &path) {
                                if self.settings.auto_switch {
                                    self.settings.auto_switch = false;
                                }
                                self.navigate_to(pos);
                            } else {
                                // Newly added file — rescan without recursive
                                self.initial_image = Some(path.clone());
                                self.settings.recursive = false;
                                if self.settings.auto_switch {
                                    self.settings.auto_switch = false;
                                }
                                self.load_directory(parent.to_path_buf());
                            }
                        } else {
                            // Different directory — scan without recursive (persisted to disk).
                            self.settings.last_image_dir = Some(parent.to_path_buf());
                            self.settings.recursive = false;
                            self.queue_save();
                            self.initial_image = Some(path.clone());
                            if self.settings.auto_switch {
                                self.settings.auto_switch = false;
                            }
                            self.load_directory(parent.to_path_buf());
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        crate::ipc::force_foreground();
                    }
                }
                IpcMessage::Focus => {
                    log::info!("IPC received empty ping, requesting window focus");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    crate::ipc::force_foreground();
                }
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(self.last_frame_time);
        self.last_frame_time = now;

        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));

        if minimized {
            // Pause the auto-switch timer while minimized by offsetting its start
            if self.settings.auto_switch {
                self.last_switch_time += dt;
            }
            
            // Limit background processing while hidden
            self.process_music_scan_results(); // Allow music to start if scanning finishes
            
            self.last_minimized = true;
            ctx.request_repaint_after(Duration::from_millis(500));
            return;
        }

        // Just restored from minimized state: force a clean UI refresh
        if self.last_minimized {
            self.last_minimized = false;
            self.last_hud_state = None; // Invalidate HUD cache to force total redraw
            ctx.request_repaint();
        }

        // Only update pixels_per_point when it actually changes
        // (e.g. window dragged to a monitor with different DPI).
        // setup_visuals() is called once at startup and on settings changes,
        // NOT every frame — it rebuilds Style/Visuals objects needlessly.
        let ppp = ctx.pixels_per_point();
        if (ppp - self.cached_pixels_per_point).abs() > 0.001 {
            self.cached_pixels_per_point = ppp;
            setup_visuals(ctx, &self.settings);
        }
        self.process_scan_results();
        self.process_music_scan_results();
        self.process_loaded_images(ctx);
        self.check_auto_switch();
        self.handle_keyboard(ctx);

        // Apply deferred viewport commands
        if let Some(fs) = self.pending_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fs));
        }

        // Keep repainting while loading, auto-switching, or playing music
        let is_music_playing = self.settings.play_music && self.cached_music_count.unwrap_or(0) > 0;
        if self.settings.auto_switch || self.scanning || !self.loader.rx.is_empty() {
            ctx.request_repaint();
        } else if is_music_playing {
            // Music only needs low-frequency polling for track-name updates (~2 fps)
            ctx.request_repaint_after(Duration::from_millis(500));
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Draw the UI. In eframe 0.34 this is the required method; `ui` is called
    /// with the root `Ui` for the window's central area.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Draw image canvas (fills the central area)
        self.draw_image_canvas_ui(ui);

        // Settings panel overlay (hidden when file assoc dialog is modal)
        #[cfg(target_os = "windows")]
        let skip_settings = self.show_file_assoc_dialog;
        #[cfg(not(target_os = "windows"))]
        let skip_settings = false;

        if self.show_settings && !skip_settings {
            self.draw_settings_panel(&ctx);
        }

        if self.show_wallpaper_dialog {
            self.draw_wallpaper_dialog(&ctx);
        }

        // File association dialog (Windows only, modal)
        #[cfg(target_os = "windows")]
        self.draw_file_assoc_dialog(&ctx);

        // Goto dialog
        if self.show_goto {
            self.draw_goto_dialog(&ctx);
        }

        // EXIF window
        if self.show_exif_window {
            // Automatic reload if window is open but cache was cleared during navigation
            if self.cached_exif_text.is_none() && !self.image_files.is_empty() {
                let path = &self.image_files[self.current_index];
                if let Some(text) = extract_exif(path) {
                    self.cached_exif_text = Some(text);
                } else {
                    self.cached_exif_text = Some("No EXIF data found in this image.".to_string());
                }
            }

            let mut close_exif = false;
            let mut close_and_copy = false;
            egui::Window::new("ℹ EXIF Information")
                .collapsible(false)
                .resizable(true)
                .default_pos(ctx.screen_rect().center() - egui::vec2(210.0, 240.0))
                .default_size([420.0, 480.0])
                .show(&ctx, |ui| {
                    if let Some(text) = &self.cached_exif_text {
                        // 1. Bottom Panel for buttons (fixed height, native separator)
                        egui::TopBottomPanel::bottom("exif_footer")
                            .resizable(false)
                            .show_inside(ui, |ui| {
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    if styled_button(ui, "📋 Copy EXIF").clicked() {
                                        close_and_copy = true;
                                    }
                                    if styled_button(ui, "Close").clicked() {
                                        close_exif = true;
                                    }
                                });
                                ui.add_space(10.0);
                            });

                        // 2. Central Area for the scrollable text
                        egui::CentralPanel::default().show_inside(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false; 2])
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.label(text);
                                });
                        });
                    } else {
                        ui.spinner();
                        ui.label("Loading metadata…");
                    }
                });
            if close_and_copy {
                if let Some(text) = &self.cached_exif_text {
                    ctx.copy_text(text.clone());
                }
                self.show_exif_window = false;
            }
            if close_exif {
                self.show_exif_window = false;
            }
        }

        // XMP window
        if self.show_xmp_window {
            if self.cached_xmp_text.is_none() && !self.image_files.is_empty() {
                let path = &self.image_files[self.current_index];
                if let Some(text) = extract_xmp(path) {
                    self.cached_xmp_text = Some(text);
                } else {
                    self.cached_xmp_text = Some("No XMP data found in this image.".to_string());
                }
            }

            let mut close_xmp = false;
            let mut close_and_copy = false;
            egui::Window::new("ℹ XMP Information")
                .collapsible(false)
                .resizable(true)
                .default_pos(ctx.screen_rect().center() - egui::vec2(260.0, 240.0))
                .default_size([520.0, 500.0])
                .show(&ctx, |ui| {
                    if let Some(text) = &self.cached_xmp_text {
                        egui::TopBottomPanel::bottom("xmp_footer")
                            .resizable(false)
                            .show_inside(ui, |ui| {
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    if styled_button(ui, "📋 Copy XMP").clicked() {
                                        close_and_copy = true;
                                    }
                                    if styled_button(ui, "Close").clicked() {
                                        close_xmp = true;
                                    }
                                });
                                ui.add_space(10.0);
                            });

                        egui::ScrollArea::vertical().show(ui, |ui| {
                            ui.add_space(10.0);
                            ui.label(RichText::new(text).monospace());
                            ui.add_space(10.0);
                        });
                    }
                });

            if close_and_copy {
                if let Some(text) = &self.cached_xmp_text {
                    ctx.copy_text(text.clone());
                }
                self.show_xmp_window = false;
            }
            if close_xmp {
                self.show_xmp_window = false;
            }
        }
    }
}

fn extract_exif(path: &std::path::Path) -> Option<String> {
    use std::fs::File;
    use std::io::BufReader;
    
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(&file);
    let exifreader = exif::Reader::new();
    let exif = exifreader.read_from_container(&mut reader).ok()?;

    let mut result = String::new();
    for f in exif.fields() {
        let tag = format!("{}", f.tag);
        let val = format!("{}", f.display_value().with_unit(&exif));
        result.push_str(&format!("{}: {}\n", tag, val));
    }
    
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn extract_xmp(path: &std::path::Path) -> Option<String> {
    use xmpkit::XmpFile;
    
    let mut file = XmpFile::new();
    if file.open(path.to_string_lossy().as_ref()).is_err() {
        return None;
    }
    
    let meta = file.get_xmp()?;
    
    let mut result = String::new();
    
    // Common Namespaces
    let namespaces = [
        ("Dublin Core", "http://purl.org/dc/elements/1.1/"),
        ("XMP Basic", "http://ns.adobe.com/xap/1.0/"),
        ("Rights Management", "http://ns.adobe.com/xap/1.0/rights/"),
        ("Media Management", "http://ns.adobe.com/xap/1.0/mm/"),
        ("Photoshop", "http://ns.adobe.com/photoshop/1.0/"),
        ("EXIF XMP", "http://ns.adobe.com/exif/1.0/"),
        ("TIFF XMP", "http://ns.adobe.com/tiff/1.0/"),
    ];

    // Common fields within those namespaces
    let common_fields = [
        "title", "creator", "description", "rights", "subject", "publisher",
        "CreateDate", "ModifyDate", "CreatorTool", "MetadataDate",
        "WebStatement", "UsageTerms",
        "DocumentID", "InstanceID",
        "AuthorsPosition", "CaptionWriter", "Category", "City", "Country", "Credit", "Headline", "Instructions", "Source", "State", "TransmissionReference",
    ];

    for (ns_name, ns_uri) in namespaces {
        let mut ns_added = false;
        for field in common_fields {
            if let Some(val) = meta.get_property(ns_uri, field) {
                if !ns_added {
                    result.push_str(&format!("--- {} ---\n", ns_name));
                    ns_added = true;
                }
                result.push_str(&format!("{}: {:?}\n", field, val));
            }
        }
        if ns_added {
            result.push_str("\n");
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup_visuals(ctx: &Context, settings: &Settings) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = PANEL_BG;
    visuals.panel_fill = BG_DARK;
    visuals.extreme_bg_color = Color32::from_rgb(12, 12, 18);

    // Non-interactive
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(20, 20, 30);
    visuals.widgets.noninteractive.weak_bg_fill = Color32::from_rgb(20, 20, 30);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(60, 60, 80));
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(150, 150, 150));

    // Inactive (default state of buttons, sliders, comboboxes)
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(20, 20, 30);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(20, 20, 30);
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(60, 60, 80));
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(240, 240, 240));

    // Hovered
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(25, 25, 35);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(25, 25, 35);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(80, 80, 100));
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);

    // Active
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.weak_bg_fill = ACCENT;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);

    // Selection
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);

    ctx.set_visuals(visuals);
    ctx.set_pixels_per_point(ctx.native_pixels_per_point().unwrap_or(1.0));

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);

    // Apply global font size
    let size = settings.font_size;
    for id in style.text_styles.values_mut() {
        id.size = size;
    }
    // Headings should be slightly larger
    if let Some(id) = style.text_styles.get_mut(&egui::TextStyle::Heading) {
        id.size = size * 1.25;
    }
    if let Some(id) = style.text_styles.get_mut(&egui::TextStyle::Small) {
        id.size = size * 0.8;
    }

    ctx.set_global_style(style);
}

/// Load a CJK-capable system font as egui fallback so Chinese/Japanese/Korean
/// characters in file paths are rendered correctly. If a specific font family is 
/// chosen in settings, try to load that one first.
fn setup_fonts(ctx: &Context, settings: &Settings) {
    let mut fonts = egui::FontDefinitions::default();
    let mut font_loaded = false;

    // 1. Try to load the user-selected font if not "System Default"
    if settings.font_family != "System Default" {
        use font_kit::source::SystemSource;
        use font_kit::family_name::FamilyName;
        use font_kit::properties::Properties;
        
        let source = SystemSource::new();
        if let Ok(handle) = source.select_best_match(
            &[FamilyName::Title(settings.font_family.clone())],
            &Properties::new(),
        ) {
            if let Ok(data) = handle.load() {
                let bytes = data.copy_font_data().map(|d| d.to_vec());
                if let Some(bytes) = bytes {
                    fonts.font_data.insert(
                        "UserFont".to_owned(),
                        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                    );
                    fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, "UserFont".to_owned());
                    fonts.families.get_mut(&egui::FontFamily::Monospace).unwrap().insert(0, "UserFont".to_owned());
                    font_loaded = true;
                }
            }
        }
    }

    // 2. Fallback to existing CJK logic if no font loaded or as secondary fallback
    #[cfg(target_os = "windows")]
    let win_fonts: String = {
        let root = std::env::var("WINDIR")
            .or_else(|_| std::env::var("SystemRoot"))
            .unwrap_or_else(|_| r"C:\Windows".to_string());
        format!(r"{}\Fonts", root)
    };
    #[cfg(target_os = "windows")]
    let candidates: Vec<String> = vec![
        format!(r"{}\msyh.ttc",   win_fonts),
        format!(r"{}\msyhbd.ttc", win_fonts),
        format!(r"{}\simsun.ttc", win_fonts),
    ];
    #[cfg(target_os = "macos")]
    let candidates: Vec<String> = vec![
        "/System/Library/Fonts/PingFang.ttc".to_string(),
        "/Library/Fonts/Arial Unicode.ttf".to_string(),
    ];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let candidates: Vec<String> = vec![
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".to_string(),
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".to_string(),
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc".to_string(),
    ];

    if let Some(data) = candidates.iter().find_map(|path| std::fs::read(path).ok()) {
        fonts.font_data.insert(
            "CJK".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(data)),
        );
        fonts.families.entry(egui::FontFamily::Proportional).or_default().push("CJK".to_owned());
        fonts.families.entry(egui::FontFamily::Monospace).or_default().push("CJK".to_owned());
        font_loaded = true;
    }

    if font_loaded {
        ctx.set_fonts(fonts);
    }
}

fn get_system_font_families() -> Vec<String> {
    use font_kit::source::SystemSource;
    let source = SystemSource::new();
    let mut families = source.all_families().unwrap_or_default();
    families.sort();
    // Insert "System Default" at the beginning
    families.insert(0, "System Default".to_string());
    families
}

fn copy_file_to_clipboard(path: &str) {
    use clipboard_rs::{Clipboard, ClipboardContext};
    if let Ok(ctx) = ClipboardContext::new() {
        let _ = ctx.set_files(vec![path.to_string()]);
    }
}

fn styled_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(Color32::WHITE))
            .fill(ACCENT)
            .corner_radius(egui::CornerRadius::same(4)),
    )
}

/// Renders a read-only path display box (Frame + Label).
/// Returns the frame's Response so callers can attach `.on_hover_text()`.
fn path_display_box(ui: &mut egui::Ui, text: &str, is_placeholder: bool, width: f32) -> egui::Response {
    let text_color = if is_placeholder {
        TEXT_MUTED
    } else {
        Color32::WHITE
    };
    let frame_resp = egui::Frame::new()
        .fill(Color32::from_rgb(20, 20, 30))
        .inner_margin(egui::Margin::symmetric(6, 4))
        .corner_radius(egui::CornerRadius::same(4))
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(60, 60, 80)))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.add(
                egui::Label::new(RichText::new(text).color(text_color).small())
                    .truncate(),
            );
        });
    frame_resp.response
}

fn draw_empty_hint(ui: &mut egui::Ui, rect: Rect) {
    ui.painter().text(
        rect.center() - Vec2::new(0.0, 12.0),
        Align2::CENTER_CENTER,
        "🖼",
        FontId::proportional(48.0),
        Color32::from_gray(60),
    );
    ui.painter().text(
        rect.center() + Vec2::new(0.0, 30.0),
        Align2::CENTER_CENTER,
        "No images loaded\nPress F1 to open settings and pick a folder",
        FontId::proportional(16.0),
        Color32::from_gray(100),
    );
}

