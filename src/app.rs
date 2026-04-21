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

use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::collections::HashMap;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{
    self, Align2, Color32, ColorImage, Context, FontId, Frame, Key, Pos2, Rect, RichText,
    Sense, TextureOptions, Vec2,
};

use crate::audio::{AudioPlayer, collect_music_files};
use crate::ipc::IpcMessage;
use crate::loader::{ImageLoader, TextureCache, LoadResult, TileResult, ImageData, DecodedImage, LoaderOutput, PreviewResult};
use crate::scanner::ScanMessage;
use crate::scanner;
pub(crate) use crate::settings::{ScaleMode, Settings, TransitionStyle};
pub(crate) use crate::theme::AppTheme;
use crate::tile_cache::{TileManager, TileCoord, TileStatus};
use crate::theme::{SystemThemeCache, ThemePalette};
use crate::ui::{settings as ui_settings, hud as ui_hud, dialogs as ui_dialogs};
use crate::ui::utils::{setup_visuals, setup_fonts, draw_empty_hint, copy_file_to_clipboard, get_system_font_families};
use rust_i18n::t;

// -- Preload configuration --
// Maximum number of images to preload in each direction.
const MAX_PRELOAD_FORWARD: usize = 5;
const MAX_PRELOAD_BACKWARD: usize = 3;
// Texture cache must hold: current + forward + backward + buffer for transitions
const CACHE_SIZE: usize = MAX_PRELOAD_FORWARD + MAX_PRELOAD_BACKWARD + 3;

/// Compute preload byte budgets based on total system RAM.
/// Forward budget = total_ram / 32, backward = total_ram / 64, both clamped.
fn compute_preload_budgets() -> (u64, u64) {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let total = sys.total_memory(); // bytes

    let forward  = (total / 32).clamp(64 * 1024 * 1024, 512 * 1024 * 1024);
    let backward = (total / 64).clamp(32 * 1024 * 1024, 256 * 1024 * 1024);

    log::info!(
        "Preload budgets: forward={} MB, backward={} MB (system RAM={} MB)",
        forward / (1024 * 1024),
        backward / (1024 * 1024),
        total / (1024 * 1024),
    );
    (forward, backward)
}

// self.cached_palette.accent colors for the UI (migrated to theme system)

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



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareTier {
    Low,
    Medium,
    High,
}

impl HardwareTier {
    pub fn max_tile_quota(&self) -> usize {
        match self {
            Self::Low => 16,
            Self::Medium => 64,
            Self::High => 128, // Reduced from 512 to avoid command queue saturation
        }
    }

    pub fn look_ahead_padding(&self) -> f32 {
        match self {
            Self::Low => 512.0,
            Self::Medium => 1024.0,
            Self::High => 2048.0,
        }
    }

    pub fn gpu_cache_tiles(&self) -> usize {
        match self {
            Self::Low => 256,     // Basic coverage
            Self::Medium => 448,   // Retina/4K coverage
            Self::High => 1024,    // Performance/Gigapixel coverage
        }
    }

    pub fn cpu_cache_mb(&self) -> usize {
        match self {
            Self::Low => 512,
            Self::Medium => 1024,
            Self::High => 2048,
        }
    }

    pub fn tiled_threshold_pixels(&self) -> u64 {
        64_000_000 // Reverted to 64MP for all tiers as requested
    }

    pub fn max_preview_size(&self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Medium => 2048,
            Self::High => crate::constants::MAX_QUALITY_PREVIEW_SIZE, // Capped at 4k to prevent VRAM spikes
        }
    }
}

pub struct ImageViewerApp {
    // Core state
    pub(crate) settings: Settings,
    pub(crate) image_files: Vec<PathBuf>,
    pub(crate) current_index: usize,
    pub(crate) initial_image: Option<PathBuf>,
    pub(crate) scanning: bool,

    // Performance tracking
    pub(crate) hardware_tier: HardwareTier,

    // Image loading
    pub(crate) loader: ImageLoader,
    pub(crate) texture_cache: TextureCache,
    /// Animated image playback state (None for static images).
    pub(crate) animation: Option<AnimationPlayback>,

    // Pan/drag state (used in non-fullscreen 1:1 mode)
    pub(crate) pan_offset: Vec2,

    // Manual zoom factor (1.0 = 100%); applied on top of any fit-to-screen scale
    pub(crate) zoom_factor: f32,

    // Auto-switch timer
    pub(crate) last_switch_time: Instant,
    pub(crate) slideshow_paused: bool,

    // Audio
    pub(crate) audio: AudioPlayer,
    pub(crate) music_seeking_target_ms: Option<u64>,
    pub(crate) music_seek_timeout: Option<std::time::Instant>,
    pub(crate) music_hud_last_activity: std::time::Instant,

    // UI state
    pub(crate) show_settings: bool,
    pub(crate) last_show_settings: bool,
    pub(crate) status_message: String,
    pub(crate) error_message: Option<String>,
    pub(crate) is_font_error: bool,

    // Pending viewport commands (set during input processing for deferred apply)
    pub(crate) pending_fullscreen: Option<bool>,

    // Cached system font families
    pub(crate) font_families: Vec<String>,
    pub(crate) temp_font_size: Option<f32>,

    // Cached state
    pub(crate) generation: u64,
    pub(crate) cached_music_count: Option<usize>,
    pub(crate) cached_pixels_per_point: f32,

    // EXIF dialog state
    pub(crate) show_exif_window: bool,
    pub(crate) cached_exif_data: Option<Vec<(String, String)>>,
    // XMP dialog state
    pub(crate) show_xmp_window: bool,
    pub(crate) cached_xmp_data: Option<Vec<(String, String)>>,
    pub(crate) cached_xmp_xml: Option<String>,

    // Goto dialog state
    pub(crate) show_goto: bool,
    pub(crate) goto_input: String,
    pub(crate) goto_needs_focus: bool,

    // Async music scanning
    pub(crate) music_scan_rx: Option<Receiver<Vec<PathBuf>>>,
    pub(crate) scanning_music: bool,
    pub(crate) music_scan_cancel: Option<Arc<AtomicBool>>,
    pub(crate) music_scan_path: Option<PathBuf>,
    pub(crate) scan_rx: Option<Receiver<scanner::ScanMessage>>,

    // Wallpaper dialog state
    pub(crate) show_wallpaper_dialog: bool,
    pub(crate) selected_wallpaper_mode: String,
    pub(crate) current_image_res: Option<(u32, u32)>,
    pub(crate) current_system_wallpaper: Option<String>,

    // File association dialog state (Windows only)
    #[cfg(target_os = "windows")]
    pub(crate) show_file_assoc_dialog: bool,
    #[cfg(target_os = "windows")]
    pub(crate) file_assoc_formats: Vec<crate::formats::ImageFormat>,
    #[cfg(target_os = "windows")]
    pub(crate) file_assoc_selections: Vec<bool>,

    // Transition state
    pub(crate) prev_texture: Option<egui::TextureHandle>,
    pub(crate) transition_start: Option<Instant>,
    pub(crate) is_next: bool,

    // OSD renderer
    pub(crate) osd: crate::ui::osd::OsdRenderer,

    // Window lifecycle
    pub(crate) last_minimized: bool,
    pub(crate) last_frame_time: Instant,

    // IPC receiver
    pub(crate) ipc_rx: crossbeam_channel::Receiver<IpcMessage>,
    
    // Predictive animation cache (decoded and uploaded to GPU)
    pub(crate) animation_cache: HashMap<usize, AnimationPlayback>,

    // Tiled rendering for large images
    pub(crate) tile_manager: Option<TileManager>,
    
    // Tiled rendering instances decoded during prefetch
    pub(crate) prefetched_tiles: HashMap<usize, TileManager>,
    
    // Theme state
    pub(crate) theme_cache: SystemThemeCache,
    pub(crate) cached_palette: ThemePalette,

    // Printing state
    pub is_printing: Arc<AtomicBool>,
    pub print_status_rx: Option<crossbeam_channel::Receiver<Option<String>>>,

    // Deferred animation frame uploads (throttled to avoid GPU stalls)
    pub(crate) pending_anim_frames: Option<PendingAnimUpload>,

    // Debounce for mouse wheel navigation
    pub(crate) last_mouse_wheel_nav: f64,

    // Settings persistence channel
    pub(crate) save_tx: Sender<Settings>,
    pub(crate) save_error_rx: Receiver<String>,
    pub(crate) last_save_error: Option<(String, Instant)>,

    // Preload byte budgets (computed at startup from system RAM)
    pub(crate) preload_budget_forward: u64,
    pub(crate) preload_budget_backward: u64,

    // Custom right-click context menu (bypasses egui's context_menu which
    // cannot re-open on consecutive right-clicks)
    pub (crate) context_menu_pos: Option<Pos2>,
    /// Current view rotation in steps of 90 degrees clockwise (0-3).
    pub (crate) current_rotation: i32,
    
    // Adaptive tile upload quota based on hardware and current frame performance
    pub (crate) tile_upload_quota: usize,

    // Audio device caching
    pub(crate) cached_audio_devices: Vec<String>,

    // Music HUD drag offset (user-adjustable position relative to default bottom-center)
    pub(crate) music_hud_drag_offset: Vec2,
}

/// Holds animation frame data waiting to be uploaded to GPU across multiple frames.
struct PendingAnimUpload {
    image_index: usize,
    frames: Vec<crate::loader::AnimationFrame>,
    textures: Vec<egui::TextureHandle>,
    delays: Vec<std::time::Duration>,
    next_frame: usize,
}

impl ImageViewerApp {
    pub fn refresh_audio_devices(&mut self) {
        log::info!("[Audio] Refreshing audio device list...");
        self.cached_audio_devices = self.audio.list_devices();
    }

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

        let mut theme_cache = SystemThemeCache::default();
        let cached_palette = settings.theme.resolve(&mut theme_cache);

        setup_visuals(&cc.egui_ctx, &settings, &cached_palette);
        if !setup_fonts(&cc.egui_ctx, &settings) {
            log::error!("[Core] Persisted font '{}' failed validation. Reverting for safety.", settings.font_family);
            // We don't have self yet, but we can't easily change settings.
            // However, setup_fonts will have at least loaded CJK as fallback.
        }

        let (save_tx, save_rx) = crossbeam_channel::unbounded::<Settings>();
        let (save_error_tx, save_error_rx) = crossbeam_channel::unbounded::<String>();
        let saver_res = std::thread::Builder::new()
            .name("settings-saver".to_string())
            .spawn(move || {
                while let Ok(mut settings) = save_rx.recv() {
                    // Coalesce rapid updates: if multiple save requests are queued (e.g., during rapid slider dragging),
                    // drain the channel and only persist the absolute latest state to avoid I/O flooding.
                    while let Ok(newer) = save_rx.try_recv() {
                        settings = newer;
                    }

                    if let Err(e) = settings.save() {
                        let _ = save_error_tx.send(e);
                    }
                    
                    // Throttling: give the OS and filesystem time to settle between writes.
                    // This prevents file locking conflicts on certain Windows/AV configurations.
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            });
        
        if saver_res.is_err() {
            log::error!("[Core] Failed to spawn settings-saver thread. Settings will not be persisted this session.");
        }

        let (budget_fwd, budget_bwd) = compute_preload_budgets();

        // ── GPU Limits ───────────────────────────────────────────────────────
        let max_texture_side_hw = cc.wgpu_render_state.as_ref()
            .map(|s| s.adapter.limits().max_texture_dimension_2d)
            .unwrap_or(crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE);
        
        // Use the hardware-reported limit directly. The wgpu device was already
        // created with this same adapter limit, so it is safe to upload textures
        // up to this size. No additional capping needed.
        let max_texture_side = max_texture_side_hw;
        log::info!("Using max_texture_side: {} (hw reported: {})", max_texture_side, max_texture_side_hw);
        
        crate::tile_cache::MAX_TEXTURE_SIDE.store(max_texture_side, std::sync::atomic::Ordering::Relaxed);
        
        // --- Hardware Tier Detection ---
        use sysinfo::System;
        let mut sys = System::new();
        sys.refresh_memory();
        let total_ram_gb = sys.total_memory() / (1024 * 1024 * 1024);
        
        let mut tier = HardwareTier::Low;
        if let Some(state) = cc.wgpu_render_state.as_ref() {
            let info = state.adapter.get_info();
            match info.device_type {
                wgpu::DeviceType::DiscreteGpu => {
                    tier = if total_ram_gb >= 16 { HardwareTier::High } else { HardwareTier::Medium };
                }
                wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::VirtualGpu => {
                    tier = if total_ram_gb >= 16 { HardwareTier::Medium } else { HardwareTier::Low };
                }
                _ => {}
            }
            log::info!("Hardware Detection: Tier={:?}, GPU={:?}, RAM={}GB, Adapter={}", 
                tier, info.device_type, total_ram_gb, info.name);
        } else {
            tier = if total_ram_gb >= 16 { HardwareTier::Medium } else { HardwareTier::Low };
            log::info!("Hardware Detection: Tier={:?} (No WGPU), RAM={}GB", tier, total_ram_gb);
        }

        let tile_quota = tier.max_tile_quota();

        // Apply hardware budgets to global caches
        crate::tile_cache::MAX_TILES_BASE.store(tier.gpu_cache_tiles(), std::sync::atomic::Ordering::Relaxed);
        crate::tile_cache::TILED_THRESHOLD.store(tier.tiled_threshold_pixels(), std::sync::atomic::Ordering::Relaxed);
        crate::loader::PREVIEW_LIMIT.store(tier.max_preview_size(), std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
            cache.set_max_mb(tier.cpu_cache_mb());
        }

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
            status_message: rust_i18n::t!("status.open_dir_hint").to_string(),
            error_message: None,
            is_font_error: false,
            pending_fullscreen: None,
            font_families: get_system_font_families(),
            temp_font_size: None,
            generation: 0,
            cached_music_count: None,
            cached_pixels_per_point: 1.0,
            show_exif_window: false,
            cached_exif_data: None,
            show_xmp_window: false,
            cached_xmp_data: None,
            cached_xmp_xml: None,
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
            file_assoc_formats: Vec::new(),
            #[cfg(target_os = "windows")]
            file_assoc_selections: Vec::new(),
            prev_texture: None,
            transition_start: None,
            is_next: true,
            osd: crate::ui::osd::OsdRenderer::new(),
            last_minimized: false,
            last_frame_time: Instant::now(),
            ipc_rx,
            animation_cache: std::collections::HashMap::new(),
            tile_manager: None,
            prefetched_tiles: std::collections::HashMap::new(),
            theme_cache,
            cached_palette,
            is_printing: Arc::new(AtomicBool::new(false)),
            print_status_rx: None,
            pending_anim_frames: None,
            last_mouse_wheel_nav: 0.0,
            preload_budget_forward: budget_fwd,
            preload_budget_backward: budget_bwd,
            context_menu_pos: None,
            current_rotation: 0,
            save_error_rx,
            last_save_error: None,
            tile_upload_quota: tile_quota,
            hardware_tier: tier,
            music_seeking_target_ms: None,
            music_seek_timeout: None,
            music_hud_last_activity: Instant::now(),
            cached_audio_devices: Vec::new(),
            last_show_settings: true,
            music_hud_drag_offset: Vec2::ZERO,
        };
        log::info!("[Core] RAW engine initialized: {}", crate::raw_processor::version());

        app.refresh_audio_devices();

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

    pub(crate) fn queue_save(&self) {
        let _ = self.save_tx.send(self.settings.clone());
    }

    // ------------------------------------------------------------------
    // Directory loading
    // ------------------------------------------------------------------

    pub(crate) fn open_directory_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(ref dir) = self.settings.last_image_dir.clone() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.load_directory(dir);
            self.queue_save();
        }
    }

    pub(crate) fn load_directory(&mut self, dir: PathBuf) {
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
        self.is_font_error = false;
        self.scanning = true;
        let dir_name = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
        self.status_message = t!("status.scanning", dir = dir_name).to_string();

        let (tx, rx) = crossbeam_channel::unbounded();
        self.scan_rx = Some(rx);
        scanner::scan_directory(dir, self.settings.recursive, tx);
    }

    // ------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------

    pub(crate) fn navigate_to(&mut self, new_index: usize) {
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

        if self.current_index != target_index {
            // Clear tiled rendering state when switching images
            self.tile_manager = None;
        }
        self.current_index = target_index;
        self.current_rotation = 0;
        self.zoom_factor = 1.0;
        self.pan_offset = Vec2::ZERO;
        self.animation = None;

        // Update resolution if already in cache (for immediate low-res display)
        if self.texture_cache.contains(self.current_index) {
            if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
                self.current_image_res = Some((w, h));
            } else if let Some(texture) = self.texture_cache.get(self.current_index) {
                let size = texture.size();
                self.current_image_res = Some((size[0] as u32, size[1] as u32));
            }
        } else {
            self.current_image_res = None;
        }

        self.last_switch_time = Instant::now();
        self.error_message = None;
        self.is_font_error = false;
        self.cached_exif_data = None;
        self.cached_xmp_data = None;

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

        // Check if we have a prefetched TileManager ready to use!
        if let Some(mut tm) = self.prefetched_tiles.remove(&self.current_index) {
            // We successfully hit the cache!
            // The prefetch completed previously (or is still decoding in background).
            // We MUST update its generation to match the current navigation sequence,
            // otherwise its internal tile queue matching will fail.
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            
            tm.generation = self.generation;
            self.current_image_res = Some((tm.full_width, tm.full_height));

            // Trigger deferred refinement now that this image is actively viewed.
            // Prefetched RAW images defer refinement to avoid ~400MB develop allocations
            // for images the user might never actually look at.
            tm.get_source().request_refinement(self.current_index, self.generation);

            self.tile_manager = Some(tm);
            
            log::info!("[App] Cache Hit: Restored prefetched TileManager for index {}", self.current_index);
        } else {
            // ALWAYS increment generation on every navigation and request a fresh load.
            // This ensures TileManager is re-initialized for large images and 
            // low-res thumbnails are upgraded to full resolution.
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.loader.request_load(
                self.current_index,
                self.generation,
                self.image_files[self.current_index].clone(),
            );
        }

        // Housekeeping: evict stale prefetched TileManagers to prevent memory leaks
        let len = self.image_files.len();
        self.prefetched_tiles.retain(|&idx, _| {
            if len == 0 {
                return false;
            }
            let dist_forward = (idx + len - self.current_index % len) % len;
            let dist_backward = (self.current_index + len - idx % len) % len;
            let circular_distance = dist_forward.min(dist_backward);
            
            // Keep tiles only within distance 2
            circular_distance <= 2
        });

        self.schedule_preloads(true);
    }

    fn print_image(&mut self, ctx: &egui::Context, mode: crate::print::PrintMode) {
        use crate::print::{PrintJob, spawn_print_job, PrintMode};
        
        if self.image_files.is_empty() { return; }
        let path = self.image_files[self.current_index].clone();
        
        if self.is_printing.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }

        let is_tiled = self.tile_manager.is_some();
        let mut crop_rect_pixels = None;
        let mut tile_pixel_buffer = None;
        let mut tile_full_width = 0u32;
        let mut tile_full_height = 0u32;

        if let Some(res) = self.current_image_res {
            let img_size = egui::vec2(res.0 as f32, res.1 as f32);
            let screen_rect = ctx.input(|i| i.content_rect()); 

            if mode == PrintMode::VisibleArea {
                let display_rect = self.compute_display_rect(img_size, screen_rect);
                let intersect = display_rect.intersect(screen_rect);
                if intersect.is_positive() {
                    let scale = img_size.x / display_rect.width(); 
                    
                    let dx = (intersect.min.x - display_rect.min.x) * scale;
                    let dy = (intersect.min.y - display_rect.min.y) * scale;
                    let dw = intersect.width() * scale;
                    let dh = intersect.height() * scale;
                    
                    crop_rect_pixels = Some([
                        dx.max(0.0) as u32,
                        dy.max(0.0) as u32,
                        dw.min(img_size.x - dx).max(1.0) as u32,
                        dh.min(img_size.y - dy).max(1.0) as u32,
                    ]);
                } else {
                    crop_rect_pixels = Some([0, 0, 1, 1]); 
                }
            }

            // For tiled images: pass the Arc'd pixel buffer (cheap clone)
            // and dimensions. The background thread will do the actual 
            // downsampling to avoid blocking the UI.
            if is_tiled {
                let tm = self.tile_manager.as_ref().unwrap();
                tile_pixel_buffer = tm.pixel_buffer_arc();
                tile_full_width = tm.full_width;
                tile_full_height = tm.full_height;
            }
        }

        let job = PrintJob {
            mode,
            original_path: path,
            crop_rect_pixels,
            is_tiled,
            tile_pixel_buffer,
            tile_full_width,
            tile_full_height,
        };

        let (tx, rx) = crossbeam_channel::unbounded();
        self.print_status_rx = Some(rx);
        spawn_print_job(job, self.is_printing.clone(), tx);
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
            // CRITICAL: Drop all resources holding the file BEFORE attempting to delete it.
            // On Windows, WIC's IStream and memmap2 will keep the file locked if we don't drop them.
            self.current_image_res = None;
            self.tile_manager = None;
            self.animation = None;
            self.texture_cache.clear();
            self.animation_cache.clear();
            self.prev_texture = None;
            
            // Yield briefly to give the OS a moment to flush handles (especially memory mapped files)
            std::thread::sleep(std::time::Duration::from_millis(20));

            let result = if permanent {
                std::fs::remove_file(&path_to_delete).map_err(|e| e.to_string())
            } else {
                trash::delete(&path_to_delete).map_err(|e| e.to_string())
            };

            if let Err(e) = result {
                self.error_message = Some(t!("status.delete_failed", err = e.to_string()).to_string());
                return;
            }
            
            // Successfully deleted
            self.image_files.remove(self.current_index);
        }

        if self.image_files.is_empty() {
            self.current_index = 0;
            self.status_message = t!("status.no_images_left").to_string();
            self.current_image_res = None;
            self.animation = None;
            self.prev_texture = None;
            self.transition_start = None;
            self.cached_exif_data = None;
            self.cached_xmp_data = None;
        } else {
            // Adjust current_index if we were at the last element
            if self.current_index >= self.image_files.len() {
                self.current_index = self.image_files.len() - 1;
            }
            
            // Reset state for new image
            self.animation = None;
            self.prev_texture = None;
            self.transition_start = None;
            self.current_rotation = 0;
            self.zoom_factor = 1.0;
            self.pan_offset = Vec2::ZERO;
            self.cached_exif_data = None;
            self.cached_xmp_data = None;
            self.error_message = None;
            self.is_font_error = false;

            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.loader.request_load(
                self.current_index,
                self.generation,
                self.image_files[self.current_index].clone(),
            );
            self.schedule_preloads(true);
        }
        
        // Force HUD update
        self.osd.invalidate();
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

        // Always load the current image
        if !self.texture_cache.contains(cur) && !self.loader.is_loading(cur) {
            let path = self.image_files[cur].clone();
            self.loader.request_load(cur, self.generation, path);
        }

        if !self.settings.preload {
            return;
        }

        // Determine the "primary" and "secondary" directions.
        // Primary gets the larger budget; secondary gets the smaller one.
        let (primary_max, primary_budget, secondary_max, secondary_budget) = if forward {
            (MAX_PRELOAD_FORWARD, self.preload_budget_forward,
             MAX_PRELOAD_BACKWARD, self.preload_budget_backward)
        } else {
            (MAX_PRELOAD_BACKWARD, self.preload_budget_backward,
             MAX_PRELOAD_FORWARD, self.preload_budget_forward)
        };

        // Collect indices for each direction
        let primary_indices: Vec<usize> = (1..=n.min(primary_max + 10)) // +10 headroom to skip tiled images
            .map(|i| if forward { (cur + i) % n } else { (cur + n - i) % n })
            .collect();

        let secondary_indices: Vec<usize> = (1..=n.min(secondary_max + 10))
            .map(|i| if forward { (cur + n - i) % n } else { (cur + i) % n })
            .collect();

        self.preload_direction(primary_indices, primary_max, primary_budget);
        self.preload_direction(secondary_indices, secondary_max, secondary_budget);
    }

    /// Preload images from a list of candidate indices, respecting count and byte limits.
    /// Rule 1: Always preload at least 1 non-tiled image (guaranteed minimum).
    /// Rule 2: Stop if count >= max_count OR cumulative NEW file size >= budget.
    /// Tiled-candidate images are skipped entirely (they use on-demand tile loading).
    /// Already-cached images occupy a count slot (preventing over-reach) but
    /// do NOT consume byte budget (no new memory allocation occurs).
    fn preload_direction(&mut self, candidates: Vec<usize>, max_count: usize, budget: u64) {
        let mut count = 0usize;
        let mut new_bytes = 0u64;

        for idx in candidates {
            if count >= max_count {
                break;
            }

            // Already cached or in-flight: occupies a slot but costs nothing new.
            if self.texture_cache.contains(idx) || self.loader.is_loading(idx) {
                count += 1;
                continue;
            }

            let path = &self.image_files[idx];

            let file_size = std::fs::metadata(path)
                .map(|m| m.len())
                .unwrap_or(0);

            // After the guaranteed first image, enforce the byte budget
            if count > 0 && new_bytes + file_size > budget {
                break;
            }

            self.loader.request_load(idx, self.generation, path.clone());
            count += 1;
            new_bytes += file_size;
        }
    }

    // ------------------------------------------------------------------
    // Background result processing
    // ------------------------------------------------------------------

    fn process_scan_results(&mut self) {
        let rx = match self.scan_rx.take() {
            Some(rx) => rx,
            None => return,
        };

        let mut done = false;

        // Drain all available messages this frame (non-blocking)
        loop {
            match rx.try_recv() {
                Ok(ScanMessage::Batch(mut batch)) => {
                    let is_first_batch = self.image_files.is_empty();
                    self.image_files.append(&mut batch);

                    let count = self.image_files.len();
                    self.status_message = t!("status.found", count = count.to_string()).to_string();

                    // On first batch: resolve initial position and start preloading immediately
                    if is_first_batch && count > 0 {
                        self.resolve_initial_position();
                        self.show_settings = false;
                        self.schedule_preloads(true);
                    }
                }
                Ok(ScanMessage::Done) => {
                    done = true;
                    self.scanning = false;

                    if self.image_files.is_empty() {
                        self.status_message = t!("status.not_found").to_string();
                    } else {
                        // Re-sort the full list now that all batches have arrived.
                        // Each batch was individually sorted, but interleaving from
                        // parallel workers means the combined list may not be sorted.
                        self.image_files.sort();

                        // Re-resolve position after global sort (indices may have shifted)
                        self.resolve_initial_position();

                        let count = self.image_files.len();
                        self.status_message = t!("status.found", count = count.to_string()).to_string();
                        self.schedule_preloads(true);
                    }
                    break;
                }
                Err(_) => break,
            }
        }

        // Put the receiver back if scanning is still in progress
        if !done {
            self.scan_rx = Some(rx);
        }
    }

    /// Resolve the starting image index from initial_image or resume settings.
    fn resolve_initial_position(&mut self) {
        if let Some(ref path) = self.initial_image {
            // Fast path: try direct path comparison first (no syscalls)
            let found = self.image_files.iter().position(|p| p == path);
            let found = found.or_else(|| {
                // Fallback: canonicalize only the target, then compare
                // with case-insensitive file names to handle path variations
                // without calling canonicalize() on every file in the list.
                let target = path.canonicalize().unwrap_or_else(|_| path.clone());
                let target_name = target.file_name()
                    .map(|n| n.to_string_lossy().to_lowercase());
                self.image_files.iter().position(|p| {
                    if let Some(ref tn) = target_name {
                        if let Some(name) = p.file_name() {
                            if name.to_string_lossy().to_lowercase() == *tn {
                                return p.parent() == target.parent()
                                    || p.canonicalize().ok().as_ref() == Some(&target);
                            }
                        }
                    }
                    false
                })
            });
            if let Some(pos) = found {
                self.current_index = pos;
            }
            self.initial_image = None;
        } else if self.settings.resume_last_image {
            let count = self.image_files.len();
            if let Some(last_path) = &self.settings.last_viewed_image {
                if let Some(pos) = self.image_files.iter().position(|p| p == last_path) {
                    self.current_index = (pos + 1) % count;
                }
            }
        }
    }
    /// Process results from the background ImageLoader.
    fn process_loaded_images(&mut self, ctx: &egui::Context) {
        // ── 1. Continue uploading deferred animation frames (max 8 per tick) ──
        const ANIM_UPLOAD_QUOTA: usize = 8;
        if let Some(ref mut pending) = self.pending_anim_frames {
            let mut uploaded = 0;
            while pending.next_frame < pending.frames.len() && uploaded < ANIM_UPLOAD_QUOTA {
                let i = pending.next_frame;
                let frame = &pending.frames[i];
                let color_image = ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.pixels,
                );
                let name = format!("anim_{}_{}", pending.image_index, i);
                let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                pending.textures.push(handle);
                pending.delays.push(frame.delay);
                pending.next_frame += 1;
                uploaded += 1;
            }

            // Check if all frames have been uploaded
            if pending.next_frame >= pending.frames.len() {
                let idx = pending.image_index;

                // Build the final AnimationPlayback from the now-complete upload
                let playback = AnimationPlayback {
                    image_index: idx,
                    textures: std::mem::take(&mut pending.textures),
                    delays: std::mem::take(&mut pending.delays),
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
                self.pending_anim_frames = None;
            } else {
                // More frames remain — ask for another repaint
                ctx.request_repaint();
            }
        }

        // ── 2. Process results from the background ImageLoader ──
        const STATIC_UPLOAD_QUOTA: usize = 2;
        let mut uploads_this_frame = 0;

        while let Some(output) = self.loader.poll() {
            match output {
                LoaderOutput::Image(load_result) => {
                    let idx = load_result.index;
                    let is_current = idx == self.current_index;
                    let gen_match = load_result.generation == self.generation;

                    // Leniency: if this is the currently viewed image, accept it even if slightly "stale"
                    if !is_current && !gen_match {
                        continue;
                    }
                    self.handle_image_load_result(load_result, ctx);
                    // Force repaint for the current image so it shows up immediately
                    if is_current {
                        ctx.request_repaint();
                    } else {
                        uploads_this_frame += 1;
                        if uploads_this_frame >= STATIC_UPLOAD_QUOTA {
                            ctx.request_repaint();
                            break;
                        }
                    }
                }
                LoaderOutput::Tile(tile_result) => {
                    self.handle_tile_load_result(tile_result, ctx);
                }
                LoaderOutput::Preview(preview_update) => {
                    self.handle_preview_update(preview_update, ctx);
                }
                LoaderOutput::Refined(idx, gen_id) => {
                    if idx == self.current_index && gen_id == self.generation {
                        log::info!("[App] Refined image notification for {}, index={}", idx, idx);
                        
                        // 1. Clear the pixel cache for this image
                        if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                            cache.remove_image(idx);
                        }

                        // 2. Bump generation and clear tile queue to force TileManager to re-pull tiles
                        // from the newly developed high-resolution buffer, discarding any blurred thumbnail tiles.
                        self.generation = self.generation.wrapping_add(1);
                        self.loader.set_generation(self.generation);
                        
                        if let Some(tm) = &mut self.tile_manager {
                            log::info!("[App] Refined: Tiled mode - forcing tile upgrade to high definition");
                            tm.generation = self.generation;
                            tm.pending_tiles.clear();
                            // Also evict the fallback preview texture
                            self.texture_cache.remove(idx);
                        } else {
                            log::warn!("[App] Refined: Static mode encountered unexpectedly. Attempting to reload.");
                            self.texture_cache.remove(idx);
                            self.loader.request_load(
                                self.current_index,
                                self.generation,
                                self.image_files[self.current_index].clone(),
                            );
                        }
                        
                        self.loader.flush_tile_queue();
                        ctx.request_repaint();
                    } else {
                        // Refinement completed for a non-current image (e.g. prefetched).
                        // Invalidate stale caches so tiles are re-extracted from the updated
                        // high-resolution buffer when the user navigates to this image.
                        log::info!("[App] Refined: background update for index {} (not current). Invalidating caches.", idx);
                        if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                            cache.remove_image(idx);
                        }
                        self.prefetched_tiles.remove(&idx);
                        self.texture_cache.remove(idx);
                    }
                }
            }
        }
    }

    fn handle_image_load_result(&mut self, load_result: LoadResult, ctx: &egui::Context) {
        let idx = load_result.index;
        match load_result.result.as_ref() {
            Ok(ImageData::Static(decoded)) => {
                let color_image = ColorImage::from_rgba_unmultiplied(
                    [decoded.width as usize, decoded.height as usize],
                    &decoded.pixels,
                );
                let name = format!("img_{}", idx);
                let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                if let Some(evicted_idx) = self.texture_cache.insert(idx, handle, decoded.width, decoded.height, false, self.current_index, self.image_files.len()) {
                    self.animation_cache.remove(&evicted_idx);
                }
                if idx == self.current_index {
                    self.current_image_res = Some((decoded.width, decoded.height));
                    if self.animation.as_ref().is_some_and(|a| a.image_index == idx) {
                        self.animation = None;
                    }
                }
            }
            Ok(ImageData::Tiled(source)) => {
                // Upload preview into texture_cache so it persists across navigations.
                // Without this, flipping away and back would re-trigger a 300ms+ load.
                if let Some(preview) = load_result.preview.as_ref() {
                    // Update texture cache if it's empty OR if it currently holds a low-res preview.
                    // This ensures we can upgrade an EXIF thumbnail to an HQ preview while protecting full static images.
                    if !self.texture_cache.contains(idx) || self.texture_cache.is_preview_placeholder(idx) {
                        let color_image = ColorImage::from_rgba_unmultiplied(
                            [preview.width as usize, preview.height as usize],
                            &preview.pixels,
                        );
                        let name = format!("img_preview_{}", idx);
                        let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                        if let Some(evicted_idx) = self.texture_cache.insert(idx, handle, source.width(), source.height(), true, self.current_index, self.image_files.len()) {
                            self.animation_cache.remove(&evicted_idx);
                        }
                    }
                }

                if idx == self.current_index {
                    self.current_image_res = Some((source.width(), source.height()));
                    crate::tile_cache::set_tile_size_for_image(source.width(), source.height());
                    let mut tm = TileManager::with_source(idx, load_result.generation, Arc::clone(&source));
                    
                    // Prefer existing cached texture (might be HQ) over the initial low-res preview
                    if let Some(cached_handle) = self.texture_cache.get(idx).cloned() {
                        tm.preview_texture = Some(cached_handle);
                    } else if let Some(preview) = load_result.preview.as_ref() {
                        self.setup_tile_manager(ctx, idx, &mut tm, preview.clone());
                    }
                    
                    self.tile_manager = Some(tm);
                    self.animation = None;
                if let Some(res) = self.current_image_res {
                    self.log_large_image(idx, res.0, res.1);
                } else {
                    log::warn!("[UI] Attempted to log large image resolution, but res was None for index {}", idx);
                }

                    // Trigger refinement ONLY for the actively-viewed image.
                    // Prefetched images stay at preview quality until navigated to.
                    source.request_refinement(idx, self.generation);
                } else {
                    // Preloading: create the TileManager and store it in prefetched_tiles
                    // so that when the user switches to this image, the source (and its 
                    // background refined RAW data) is immediately available!
                    let mut tm = TileManager::with_source(idx, load_result.generation, Arc::clone(source));
                    
                    // Prefer existing cached texture (might be HQ) over the initial low-res preview
                    if let Some(cached_handle) = self.texture_cache.get(idx).cloned() {
                        tm.preview_texture = Some(cached_handle);
                    } else if let Some(preview) = load_result.preview.as_ref() {
                        self.setup_tile_manager(ctx, idx, &mut tm, preview.clone());
                    }
                    self.prefetched_tiles.insert(idx, tm);
                }
            }
            Ok(ImageData::Animated(frames)) => {
                // Upload first frame immediately
                if let Some(first) = frames.first() {
                    let color_image = ColorImage::from_rgba_unmultiplied(
                        [first.width as usize, first.height as usize],
                        &first.pixels,
                    );
                    let name = format!("img_{}", idx);
                    let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                    if let Some(evicted_idx) = self.texture_cache.insert(idx, handle, first.width, first.height, false, self.current_index, self.image_files.len()) {
                        self.animation_cache.remove(&evicted_idx);
                    }
                    if idx == self.current_index {
                        self.current_image_res = Some((first.width, first.height));
                    }
                }

                // Defer remaining
                let cur = self.current_index;
                let n = self.image_files.len();
                let is_in_range = if n > 0 {
                    idx == cur || idx == (cur + 1) % n || (cur > 0 && idx == cur - 1) || (cur == 0 && idx == n - 1)
                } else { false };

                if is_in_range {
                    self.pending_anim_frames = Some(PendingAnimUpload {
                        image_index: idx,
                        frames: frames.clone(),
                        textures: Vec::new(),
                        delays: Vec::new(),
                        next_frame: 0,
                    });
                    ctx.request_repaint();
                }
            }
            Err(e) => {
                let path_str = self.image_files[idx].display().to_string();
                log::error!("Failed to load image at index {} ({}): {e}", idx, path_str);
                if idx == self.current_index {
                    self.error_message = Some(t!("status.load_failed", path = path_str, err = e.to_string()).to_string());
                }
            }
        }
    }

    fn handle_tile_load_result(&mut self, tile_result: TileResult, _ctx: &egui::Context) {
        let coord = TileCoord { col: tile_result.col, row: tile_result.row };
        
        // Pixels are already in PIXEL_CACHE (inserted by the worker thread).
        // We only need to mark as no longer pending and trigger repaint for GPU upload.
        if let Some(ref mut tm) = self.tile_manager {
            if tm.image_index == tile_result.index {
                tm.pending_tiles.remove(&coord);
                // Trigger repaint so the next frame uploads this to GPU immediately
                _ctx.request_repaint();
            }
        }
    }


    fn handle_preview_update(&mut self, update: PreviewResult, ctx: &egui::Context) {
        // Apply HQ preview if it matches the currently displayed tile manager.
        // Also check prefetched tiles and update the texture cache for future navigations.
        match update.result {
            Ok(preview) => {
                // 1. Update current TileManager
                if let Some(ref mut tm) = self.tile_manager {
                    if tm.image_index == update.index {
                        log::info!("[App] HQ preview applied for current index {} ({}x{})", 
                            update.index, preview.width, preview.height);
                        tm.set_preview(preview.clone(), ctx);
                        ctx.request_repaint();
                    }
                }

                // 2. Update prefetched TileManagers
                if let Some(tm) = self.prefetched_tiles.get_mut(&update.index) {
                    log::info!("[App] HQ preview applied for prefetched index {} ({}x{})", 
                        update.index, preview.width, preview.height);
                    tm.set_preview(preview.clone(), ctx);
                }

                // 3. Update global texture cache (so instant-flips also get HQ texture).
                // Only update if it's empty or currently holds a preview (don't downgrade full static images).
                if !self.texture_cache.contains(update.index) || self.texture_cache.is_preview_placeholder(update.index) {
                    // Preserve the TRUE image dimensions (e.g. 11648×8736) when updating the preview texture.
                    // Without this, a small preview (e.g. 160×120 EXIF thumbnail) would overwrite
                    // original_res, causing the OSD to display wildly wrong zoom percentages (e.g. 16000%).
                    let (orig_w, orig_h) = self.texture_cache.get_original_res(update.index)
                        .unwrap_or((preview.width, preview.height));

                    let name = format!("img_hq_preview_{}", update.index);
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [preview.width as usize, preview.height as usize],
                        &preview.pixels,
                    );
                    let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                    self.texture_cache.insert(
                        update.index,
                        handle,
                        orig_w,
                        orig_h,
                        true, // is_tiled
                        self.current_index,
                        self.image_files.len(),
                    );
                }
            }
            Err(e) => {
                log::error!("Preview update failed for index {}: {}", update.index, e);
            }
        }
    }

    fn log_large_image(&self, idx: usize, w: u32, h: u32) {
        let file_name = self.image_files[idx].file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        log::info!(
            "[{}] Large image detected: {}x{} ({:.1} MP) — tiled mode active",
            file_name, w, h,
            (w as f64 * h as f64) / 1_000_000.0
        );
    }

    fn setup_tile_manager(&self, ctx: &egui::Context, idx: usize, tm: &mut TileManager, preview: DecodedImage) {
        let preview_img = egui::ColorImage::from_rgba_unmultiplied(
            [preview.width as usize, preview.height as usize],
            &preview.pixels,
        );
        let preview_handle = ctx.load_texture(
            format!("preview_{}", idx),
            preview_img,
            egui::TextureOptions::LINEAR,
        );
        tm.preview_texture = Some(preview_handle);
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
                    
                    // Try to resume from last played track
                    let mut start_idx = None;
                    if let Some(last_path) = &self.settings.last_music_file {
                        if let Some(idx) = files.iter().position(|p| p == last_path) {
                            start_idx = Some(idx);
                        }
                    }
                    
                    let start_track_idx = if start_idx.is_some() { self.settings.last_music_cue_track } else { None };
                    self.audio.start_at(files, start_idx, start_track_idx);
                    self.audio.set_volume(self.settings.volume);
                    if self.settings.music_paused {
                        self.audio.pause();
                    }
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
        let mut scroll_delta = egui::Vec2::ZERO;
        let mut zoom_delta = 1.0_f32;
        let mut is_ctrl_pressed = false;
        let mut is_alt_pressed = false;
        let mut mouse_pos: Option<egui::Pos2> = None;
        let mut toggle_auto_switch = false;
        let mut toggle_goto = false;
        let mut do_refresh = false;
        #[allow(unused_mut)]
        let mut do_quit = false;
        let mut do_delete = false;
        let mut do_permanent_delete = false;
        let mut do_print_full = false;
        let mut rotate_ccw = false;
        let mut rotate_cw = false;

        // Collect all modal flags to prevent deletion when a dialog is active
        // Collect all modal flags to prevent interaction when a dialog is active
        let any_modal_open = self.show_wallpaper_dialog 
            || self.show_goto 
            || self.show_exif_window
            || self.show_xmp_window;

        #[cfg(target_os = "windows")]
        let any_modal_open = any_modal_open || self.show_file_assoc_dialog;

        ctx.input(|i| {
            if i.key_pressed(Key::F5) {
                do_refresh = true;
            }
            if i.key_pressed(Key::Space) {
                toggle_auto_switch = true;
            }
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::PageDown) {
                nav_next = true;
            }
            if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::PageUp) {
                nav_prev = true;
            }
            if i.key_pressed(Key::Home) {
                nav_first = true;
            }
            if i.key_pressed(Key::End) {
                nav_last = true;
            }
            // F1 is the ONLY key to toggle settings/options.
            if i.key_pressed(Key::F1) {
                toggle_settings = true;
            }
            // Escape: close modals or currently open settings. NEVER opens settings from main view.
            if i.key_pressed(Key::Escape) {
                if any_modal_open || self.show_settings {
                    toggle_settings = true; 
                } else if self.settings.fullscreen {
                    toggle_fullscreen = true;
                }
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
            // Mouse wheel collected here, guarded before application below
            scroll_delta = i.smooth_scroll_delta;
            zoom_delta = i.zoom_delta();
            is_ctrl_pressed = i.modifiers.command;
            is_alt_pressed = i.modifiers.alt;
            mouse_pos = i.pointer.latest_pos();
            // F11 / F — toggle fullscreen
            if i.key_pressed(Key::F11) || i.key_pressed(Key::F) {
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
            // Rotation shortcuts: Ctrl+Left / Ctrl+Right
            if i.modifiers.command {
                if i.key_pressed(Key::ArrowLeft) {
                    rotate_ccw = true;
                    nav_prev = false; // Override navigation
                }
                if i.key_pressed(Key::ArrowRight) {
                    rotate_cw = true;
                    nav_next = false; // Override navigation
                }
            }
            if !any_modal_open {
                if i.modifiers.command && i.key_pressed(Key::P) {
                    do_print_full = true;
                }
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
        if do_print_full { self.print_image(ctx, crate::print::PrintMode::FullImage); }

        if !any_modal_open {
            if do_refresh { self.load_directory(self.settings.last_image_dir.clone().unwrap_or_default()); }
            if nav_next { self.navigate_next(); }
            if nav_prev { self.navigate_prev(); }
            if nav_first { self.navigate_first(); }
            if nav_last { self.navigate_last(); }

            if zoom_in {
                self.zoom_factor = (self.zoom_factor * 1.1).min(20.0);
                self.generation = self.generation.wrapping_add(1);
                self.loader.set_generation(self.generation);
                if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                self.loader.flush_tile_queue();
            }
            if zoom_out {
                self.zoom_factor = (self.zoom_factor / 1.1).max(0.05);
                self.generation = self.generation.wrapping_add(1);
                self.loader.set_generation(self.generation);
                if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                self.loader.flush_tile_queue();
            }
            if zoom_reset {
                self.zoom_factor = 1.0;
                self.pan_offset = Vec2::ZERO;
                self.generation = self.generation.wrapping_add(1);
                self.loader.set_generation(self.generation);
                if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                self.loader.flush_tile_queue();
            }
        }
        if toggle_settings {
            #[cfg(target_os = "windows")]
            if self.show_file_assoc_dialog {
                self.show_file_assoc_dialog = false;
            } else if self.show_exif_window {
                self.show_exif_window = false;
            } else if self.show_xmp_window {
                self.show_xmp_window = false;
            } else if self.show_wallpaper_dialog {
                self.show_wallpaper_dialog = false;
            } else if self.show_goto {
                self.show_goto = false;
            } else {
                self.show_settings = !self.show_settings;
            }
            #[cfg(not(target_os = "windows"))]
            {
                if self.show_exif_window {
                    self.show_exif_window = false;
                } else if self.show_xmp_window {
                    self.show_xmp_window = false;
                } else if self.show_wallpaper_dialog {
                    self.show_wallpaper_dialog = false;
                } else if self.show_goto {
                    self.show_goto = false;
                } else {
                    self.show_settings = !self.show_settings;
                }
            }
        }

        let ui_consuming_scroll = any_modal_open || self.show_settings || ctx.egui_wants_pointer_input();
        if !ui_consuming_scroll {
            if is_alt_pressed && scroll_delta.y.abs() > 0.0 {
                // Rotation with Alt + Mouse Wheel (steps of 90 degrees)
                let now = ctx.input(|i| i.time);
                if now - self.last_mouse_wheel_nav > 0.2 { // Reuse cooldown to prevent spinning
                    if scroll_delta.y > 0.0 {
                        rotate_ccw = true;
                    } else if scroll_delta.y < 0.0 {
                        rotate_cw = true;
                    }
                    self.last_mouse_wheel_nav = now;
                }
            } else if is_ctrl_pressed {
                // Zoom-to-cursor...
                if zoom_delta != 1.0 {
                    let old_zoom = self.zoom_factor;
                    self.zoom_factor = (self.zoom_factor * zoom_delta).clamp(0.05, 20.0);
                    let ratio = self.zoom_factor / old_zoom;

                    if let Some(mouse) = mouse_pos {
                        let screen_center = ctx.input(|i| i.content_rect()).center();
                        let d = mouse - screen_center;
                        // d * (1 - ratio) compensates for the scale change around the cursor
                        self.pan_offset = d * (1.0 - ratio) + self.pan_offset * ratio;
                    }
                    
                    self.generation = self.generation.wrapping_add(1);
                    self.loader.set_generation(self.generation);
                    if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                    self.loader.flush_tile_queue();
                }
            } else if scroll_delta.y.abs() > 0.0 {
                // Navigation with debounce (cooldown) to prevent rapid flipping
                let now = ctx.input(|i| i.time);
                if now - self.last_mouse_wheel_nav > 0.2 { // 200ms cooldown
                    if scroll_delta.y > 0.0 {
                        self.navigate_prev();
                    } else {
                        self.navigate_next();
                    }
                    self.last_mouse_wheel_nav = now;
                }
            }
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
        if toggle_auto_switch && !self.show_settings {
            if self.settings.auto_switch {
                self.slideshow_paused = !self.slideshow_paused;
                if !self.slideshow_paused {
                    self.last_switch_time = Instant::now();
                }
            }
            // If auto_switch is OFF, space does nothing — user must enable it via settings.
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

        // Apply rotation if requested (by keys OR mouse wheel)
        if rotate_ccw { self.apply_rotation_with_tracking(false, ctx); }
        if rotate_cw { self.apply_rotation_with_tracking(true, ctx); }
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

    pub(crate) fn open_music_file_dialog(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("Music files", &["mp3", "flac", "ogg", "wav", "aac", "m4a", "ape"]);
        if let Some(path) = dialog.pick_file() {
            self.settings.music_path = Some(path.clone());
            self.restart_audio_if_enabled();
        }
    }

    pub(crate) fn open_music_dir_dialog(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.settings.music_path = Some(dir.clone());
            self.restart_audio_if_enabled();
        }
    }

    pub(crate) fn restart_audio_if_enabled(&mut self) {
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

            // Background scan — do NOT block the UI
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
    // Audio: Force restart after hardware stall
    // ------------------------------------------------------------------

    /// Force a full audio restart, bypassing the "already scanned" guard.
    /// Used when the audio watchdog detects a hardware stall.
    fn force_restart_audio(&mut self) {
        // Stop audio and clear ALL scan state so restart_audio_if_enabled
        // doesn't short-circuit with "already scanning this path".
        if let Some(cancel) = self.music_scan_cancel.take() {
            cancel.store(false, Ordering::Relaxed);
        }
        self.audio.stop();
        self.scanning_music = false;
        self.music_scan_rx = None;
        self.music_scan_path = None;
        self.cached_music_count = None;

        // Now trigger a full restart (will re-scan and SetPlaylist)
        self.restart_audio_if_enabled();
    }

    // ------------------------------------------------------------------
    // UI: Settings panel
    // ------------------------------------------------------------------

    fn draw_settings_panel(&mut self, ctx: &egui::Context) {
        ui_settings::draw(self, ctx);
    }

    fn draw_wallpaper_dialog(&mut self, ctx: &egui::Context) {
        ui_dialogs::wallpaper::draw(self, ctx);
    }

    fn draw_goto_dialog(&mut self, ctx: &egui::Context) {
        ui_dialogs::goto::draw(self, ctx);
    }

    #[cfg(target_os = "windows")]
    fn draw_file_assoc_dialog(&mut self, ctx: &egui::Context) {
        ui_dialogs::file_assoc::draw(self, ctx);
    }

    fn draw_music_hud_foreground(&mut self, ctx: &egui::Context) {
        ui_hud::draw(self, ctx);
    }

    // ------------------------------------------------------------------
    // UI: Image canvas
    // ------------------------------------------------------------------

    /// Shared content for the right-click context menu (used by the custom
    /// `egui::Area`-based popup in [`Self::draw_image_canvas_ui`]).
    fn draw_context_menu_items(&mut self, ui: &mut egui::Ui) {
        let path = &self.image_files[self.current_index];
        let path_str = path.to_string_lossy().to_string();

        if ui.button(t!("ctx.copy_path").to_string()).clicked() {
            ui.ctx().copy_text(path_str.clone());
            self.context_menu_pos = None;
        }

        if ui.button(t!("ctx.copy_file").to_string()).clicked() {
            copy_file_to_clipboard(&path_str);
            self.context_menu_pos = None;
        }

        ui.separator();

        if ui.button(t!("ctx.view_exif").to_string()).clicked() {
            self.cached_exif_data = extract_exif(path);
            self.show_exif_window = true;
            self.context_menu_pos = None;
        }

        if ui.button(t!("ctx.view_xmp").to_string()).clicked() {
            self.show_xmp_window = true;
            self.context_menu_pos = None;
        }

        ui.separator();

        if ui.button(t!("ctx.rotate_ccw").to_string()).clicked() {
            self.apply_rotation_with_tracking(false, ui.ctx());
            self.context_menu_pos = None;
        }
        
        if ui.button(t!("ctx.rotate_cw").to_string()).clicked() {
            self.apply_rotation_with_tracking(true, ui.ctx());
            self.context_menu_pos = None;
        }

        ui.separator();
        if ui
            .button(if cfg!(not(target_os = "windows")) {
                t!("ctx.print_pdf_full").to_string()
            } else {
                t!("ctx.print_full").to_string()
            })
            .clicked()
        {
            self.print_image(ui.ctx(), crate::print::PrintMode::FullImage);
            self.context_menu_pos = None;
        }
        if ui
            .button(if cfg!(not(target_os = "windows")) {
                t!("ctx.print_pdf_visible").to_string()
            } else {
                t!("ctx.print_visible").to_string()
            })
            .clicked()
        {
            self.print_image(ui.ctx(), crate::print::PrintMode::VisibleArea);
            self.context_menu_pos = None;
        }

        ui.separator();
        if ui
            .button(t!("ctx.set_wallpaper").to_string())
            .clicked()
        {
            self.show_wallpaper_dialog = true;
            if let Ok(p) = wallpaper::get() {
                self.current_system_wallpaper = Some(p);
            } else {
                self.current_system_wallpaper = Some("Unknown".to_string());
            }
            self.context_menu_pos = None;
        }

        ui.separator();
        let fs_label = if self.settings.fullscreen {
            t!("ctx.fullscreen_exit").to_string()
        } else {
            t!("ctx.fullscreen_enter").to_string()
        };
        if ui.button(fs_label).clicked() {
            self.settings.fullscreen = !self.settings.fullscreen;
            self.pending_fullscreen = Some(self.settings.fullscreen);
            self.queue_save();
            self.context_menu_pos = None;
        }
    }

    fn draw_image_canvas_ui(&mut self, ui: &mut egui::Ui) {
        // Collect modal flags to block mouse interaction
        let any_modal_open = self.show_wallpaper_dialog 
            || self.show_goto 
            || self.show_exif_window
            || self.show_xmp_window;
        #[cfg(target_os = "windows")]
        let any_modal_open = any_modal_open || self.show_file_assoc_dialog;

        // Fill the area with dark background
        egui::Frame::NONE.fill(self.cached_palette.canvas_bg).show(ui, |ui| {
            let screen_rect = ui.max_rect();
            
            // Allocate the whole viewport for drag interaction and clicks early
            // If a modal is open, we sense nothing to block background clicks/drags
            let sense = if any_modal_open { Sense::hover() } else { Sense::click_and_drag() };
            let canvas_resp = ui.allocate_rect(screen_rect, sense);

            // ── Custom right-click context menu ──────────────────────────
            // We bypass `response.context_menu()` entirely because egui's
            // popup layer consumes the secondary-click event when it closes
            // an existing menu, making it impossible to re-open the menu
            // with a single right-click.  Instead we detect raw right-clicks
            // via `ctx.input()` and render the menu through `egui::Area`.
            if !any_modal_open && !self.image_files.is_empty() {
                let ctx = ui.ctx().clone();
                let raw_secondary = ctx.input(|i| i.pointer.secondary_clicked());
                let interact_pos  = ctx.input(|i| i.pointer.interact_pos());

                // Open or reposition on right-click inside the canvas
                if raw_secondary && canvas_resp.hovered() {
                    if let Some(pos) = interact_pos {
                        self.context_menu_pos = Some(pos);
                    }
                }

                // Close on Escape
                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.context_menu_pos = None;
                }

                // Render the popup and handle close-on-click / button actions
                if let Some(pos) = self.context_menu_pos {
                    let area_resp = egui::Area::new(egui::Id::new("__custom_canvas_ctx_menu"))
                        .kind(egui::UiKind::Menu)
                        .order(egui::Order::Foreground)
                        .fixed_pos(pos)
                        .default_width(ctx.global_style().spacing.menu_width)
                        .sense(Sense::hover())
                        .show(&ctx, |ui| {
                            egui::Frame::menu(ui.style()).show(ui, |ui| {
                                ui.with_layout(
                                    egui::Layout::top_down_justified(egui::Align::LEFT),
                                    |ui| self.draw_context_menu_items(ui),
                                );
                            });
                        });

                    let menu_rect = area_resp.response.rect;

                    // Close if the user clicked (primary) outside the menu
                    let primary_clicked = ctx.input(|i| i.pointer.primary_clicked());
                    if primary_clicked {
                        if let Some(pp) = interact_pos {
                            if !menu_rect.contains(pp) {
                                self.context_menu_pos = None;
                            }
                        }
                    }

                    // Close if a button inside called ui.close()
                    if area_resp.response.should_close() {
                        self.context_menu_pos = None;
                    }
                }
            }

            // Draw a dimmer rect if a modal is open
            if any_modal_open {
                ui.painter().rect_filled(screen_rect, 0.0, Color32::from_black_alpha(150));
            }
            
            if self.show_settings && canvas_resp.clicked_by(egui::PointerButton::Primary) {
                self.show_settings = false;
            }

            if self.image_files.is_empty() {
                draw_empty_hint(ui, screen_rect, &self.cached_palette);
                return;
            }

            // Error message
            if let Some(ref err) = self.error_message {
                // EXCLUSION: If the settings panel is open and showing the font error inline, 
                // skip the global centered area to avoid overlap.
                if self.show_settings && self.is_font_error {
                    // Rendered inline in draw_settings_panel
                } else {
                    // If it's a font error, always use dynamic translation. 
                    // Otherwise use the stored 'baked' string (which might contain dynamic data like paths).
                    let text = if self.is_font_error {
                        format!("⚠ {}", t!("status.invalid_font"))
                    } else {
                        format!("⚠ {err}")
                    };
                    let font_id = FontId::proportional(16.0);
                    let color = Color32::from_rgb(255, 100, 100);

                    egui::Area::new("error_display".into())
                    .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                    .show(ui.ctx(), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(text).font(font_id).color(color))
                                .selectable(true)
                                .halign(egui::Align::Center)
                        );
                    });
                return;
                }
            }

            // ── Tiled rendering path (large images) ──────────────────────
            if self.tile_manager.is_some() {
                if canvas_resp.dragged() {
                    self.pan_offset += canvas_resp.drag_delta();
                    self.generation = self.generation.wrapping_add(1);
                    self.loader.set_generation(self.generation);
                    if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                    self.loader.flush_tile_queue();
                }

                // Rotation logic
                let rotation = self.current_rotation;
                let needs_swap = rotation % 2 != 0;
                let angle = rotation as f32 * (std::f32::consts::PI / 2.0);

                // Extract immutable data first (avoids borrow conflict with compute_display_rect)
                let tm_ref = self.tile_manager.as_ref().unwrap();
                let img_size = Vec2::new(tm_ref.full_width as f32, tm_ref.full_height as f32);
                
                let rotated_img_size = if needs_swap { Vec2::new(img_size.y, img_size.x) } else { img_size };
                let dest = self.compute_display_rect(rotated_img_size, screen_rect);

                // The painter transform will handle the actual rotation.
                // We need to draw the UNROTATED image into a rect that, when rotated, matches 'dest'.
                let unrotated_size = if needs_swap { Vec2::new(dest.height(), dest.width()) } else { dest.size() };
                let unrotated_dest = Rect::from_center_size(dest.center(), unrotated_size);

                // 1. Draw preview texture as blurry background
                if let Some(ref preview) = self.tile_manager.as_ref().unwrap().preview_texture {
                    let mut mesh = egui::Mesh::with_texture(preview.id());
                    let color = Color32::WHITE;
                    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                    mesh.add_rect_with_uv(unrotated_dest, uv, color);
                    
                    if rotation != 0 {
                        let pivot = dest.center();
                        let rot = egui::emath::Rot2::from_angle(angle);
                        for v in &mut mesh.vertices {
                            v.pos = pivot + rot * (v.pos - pivot);
                        }
                    }
                    ui.painter().with_clip_rect(screen_rect).add(egui::Shape::mesh(mesh));
                }

                // 2. Render high-res tiles.
                // We use a dynamic threshold: Never trigger tiling in "Fit to Window" mode (regardless of image size).
                // For giant images, we also only trigger tiling when the effective scale exceeds 
                // the preview scale, ensuring we don't thrash VRAM for no visual gain.
                let fit_scale = (screen_rect.width() / rotated_img_size.x)
                    .min(screen_rect.height() / rotated_img_size.y)
                    .min(1.0);
                
                // preview_scale: ratio of preview texture resolution to the ORIGINAL image resolution.
                // This tells us at what display scale the preview's native pixels would be 1:1.
                // Above this scale, tiles provide higher quality than the preview.
                let preview_scale = if let Some(ref p) = tm_ref.preview_texture {
                    p.size()[0] as f32 / rotated_img_size.x.max(1.0)
                } else {
                    0.1 // Fallback
                };

                // Trigger tiling when the display resolution exceeds the preview's native resolution.
                // Two scenarios:
                // 1. HQ preview available (preview_scale >= fit_scale): tile when zoomed past preview quality
                // 2. LQ bootstrap preview (preview_scale < fit_scale): use conservative threshold to avoid
                //    flooding the queue with thousands of tiles before HQ preview arrives
                let threshold = if preview_scale >= fit_scale {
                    // Tile when zoomed sufficiently past preview's native resolution.
                    // At preview_scale * 1.0, tiles offer no visible improvement over the preview.
                    // At 1.2x, tiles are noticeably sharper while keeping tile count manageable.
                    (preview_scale * 1.2).max(fit_scale * 1.05)
                } else {
                    // LQ bootstrap: require tiles to render at >= 64 screen pixels before loading
                    let min_tile_screen_px = 64.0;
                    let tile_scale_min = min_tile_screen_px / crate::tile_cache::get_tile_size() as f32;
                    tile_scale_min.max(fit_scale * 1.05)
                };

                let effective_scale = dest.width() / rotated_img_size.x;
                
                // Log threshold diagnostics once per image load
                {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static LAST_LOGGED_SCALE: AtomicU64 = AtomicU64::new(0);
                    let scale_bits = (effective_scale * 1000.0) as u64;
                    let prev = LAST_LOGGED_SCALE.load(Ordering::Relaxed);
                    if scale_bits != prev {
                        LAST_LOGGED_SCALE.store(scale_bits, Ordering::Relaxed);
                        if effective_scale >= threshold * 0.9 && effective_scale <= threshold * 1.1 {
                            let fname = self.image_files[self.current_index].file_name()
                                .and_then(|n| n.to_str()).unwrap_or("?");
                            log::info!("[Tiling] [{}] preview_scale={:.4}, fit_scale={:.4}, threshold={:.4}, effective={:.4}, img_w={}, tiled={}",
                                fname, preview_scale, fit_scale, threshold, effective_scale, rotated_img_size.x as u32, effective_scale >= threshold);
                        }
                    }
                }
                
                if effective_scale >= threshold {
                    // Compute visible tiles using the UNROTATED destination rect.
                    // When rotation is active, we must inverse-rotate the screen clip
                    // region into unrotated coordinate space. Otherwise, for extremely
                    // tall/narrow images rotated 90°/270°, the unrotated rect is narrow
                    // and its intersection with screen_rect only covers the center tiles.
                    let padding = self.hardware_tier.look_ahead_padding();
                    let tile_clip = if rotation != 0 {
                        let inv_rot = egui::emath::Rot2::from_angle(-angle);
                        let pivot = dest.center();
                        let corners = [
                            screen_rect.left_top(),
                            screen_rect.right_top(),
                            screen_rect.right_bottom(),
                            screen_rect.left_bottom(),
                        ].map(|p| pivot + inv_rot * (p - pivot));
                        // Compute the axis-aligned bounding box of the rotated corners
                        let min_x = corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
                        let max_x = corners.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
                        let min_y = corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
                        let max_y = corners.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
                        Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y))
                    } else {
                        screen_rect
                    };
                    let visible = self.tile_manager.as_ref().unwrap().visible_tiles(unrotated_dest, tile_clip, padding);
                    
                    // ANTI-THRASHING: We no longer truncate 'visible' here.
                    // Eviction logic is now handled in get_or_create_tile to prevent circular holes.
                    // visible.truncate(self.hardware_tier.gpu_cache_tiles());

                    // Upload and draw tiles (mutable borrow, scoped)
                    let ctx_ref = ui.ctx().clone();

                    // BURST POLICY:
                    // If we are NOT dragging and NOT scrolling (stable view), boost upload quota
                    // to fill the screen quickly. Otherwise, keep it low to maintain 60FPS.
                    let is_interacting = canvas_resp.dragged() || self.last_mouse_wheel_nav.abs() > 0.01;
                    let tile_upload_quota = if !is_interacting {
                        (self.tile_upload_quota * 4).min(48) // Burst mode
                    } else {
                        self.tile_upload_quota // Stable mode
                    };
                    
                    let mut newly_uploaded = 0;

                    {
                        let tm = self.tile_manager.as_mut().unwrap();
                        let pivot = dest.center();
                        let rot = if rotation != 0 { Some(egui::emath::Rot2::from_angle(angle)) } else { None };

                        let visible_coords: Vec<TileCoord> = visible.iter().map(|(c, _, _)| *c).collect();
                        for (idx, (coord, tile_screen_rect, uv)) in visible.iter().enumerate() {
                            let allow_upload = newly_uploaded < tile_upload_quota;
                            let (status, just_uploaded) = tm.get_or_create_tile(*coord, &ctx_ref, allow_upload, &visible_coords);
                            
                            if just_uploaded {
                                newly_uploaded += 1;
                            }

                            match status {
                                TileStatus::Ready(handle, ready_at) => {
                                    let mut alpha = 1.0;
                                    if let Some(at) = ready_at {
                                        let elapsed = at.elapsed().as_secs_f32();
                                        let duration = 0.2; // 200ms smooth fade
                                        if elapsed < duration {
                                            alpha = (elapsed / duration).clamp(0.0, 1.0);
                                            ui.ctx().request_repaint(); // Smooth transition
                                        }
                                    }

                                    let color = Color32::WHITE.linear_multiply(alpha);
                                    let mut mesh = egui::Mesh::with_texture(handle.id());
                                    mesh.add_rect_with_uv(*tile_screen_rect, *uv, color);
                                    if let Some(r) = rot {
                                        for v in &mut mesh.vertices {
                                            v.pos = pivot + r * (v.pos - pivot);
                                        }
                                    }
                                    ui.painter().with_clip_rect(screen_rect).add(egui::Shape::mesh(mesh));
                                    
                                    // DEBUG: Visual confirmation of high-res tile placement
                                    #[cfg(feature = "tile-debug")]
                                    if self.settings.show_osd {
                                        let debug_rect = *tile_screen_rect;
                                        if let Some(r) = rot {
                                            // Approximate rotation of rect for border
                                            let p1 = pivot + r * (debug_rect.left_top() - pivot);
                                            let p2 = pivot + r * (debug_rect.right_top() - pivot);
                                            let p3 = pivot + r * (debug_rect.right_bottom() - pivot);
                                            let p4 = pivot + r * (debug_rect.left_bottom() - pivot);
                                            ui.painter().line_segment([p1, p2], egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)));
                                            ui.painter().line_segment([p2, p3], egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)));
                                            ui.painter().line_segment([p3, p4], egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)));
                                            ui.painter().line_segment([p4, p1], egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)));
                                        } else {
                                            ui.painter().rect(debug_rect, 0.0, Color32::TRANSPARENT, egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)), egui::StrokeKind::Inside);
                                        }
                                    }
                                }
                                TileStatus::Pending(needs_request) => {
                                    if needs_request {
                                        // Dynamic pending cap: scale inversely with visible tile count.
                                        // At high zoom (few tiles visible), load fast.
                                        // At low zoom (many visible), allow enough to keep worker threads busy.
                                        // Scale down for larger tiles to keep memory bounded.
                                        let visible_count = visible.len();
                                        let ts = crate::tile_cache::get_tile_size();
                                        let scale = if ts >= 1024 { 2 } else { 1 }; // halve caps for 1024 tiles
                                        let max_pending = if visible_count > 1000 {
                                            24 / scale
                                        } else if visible_count > 200 {
                                            48 / scale
                                        } else if visible_count > 50 {
                                            64 / scale
                                        } else {
                                            96 / scale
                                        };
                                        if tm.pending_tiles.len() >= max_pending {
                                            continue; // Don't break — still need to draw already-Ready tiles below
                                        }
                                        let source = tm.get_source();
                                        let generation = tm.generation;
                                        // visible list is already sorted by distance to center
                                        let priority = (visible.len() - idx) as f32;
                                        self.loader.request_tile(self.current_index, generation, priority, source, coord.col, coord.row);
                                        tm.pending_tiles.insert(*coord);
                                    }
                                }
                            }
                        }
                    }
                    
                    // DEBUG HUD: real-time tiled rendering diagnostics
                    #[cfg(feature = "tile-debug")]
                    if self.settings.show_osd {
                        let visible_coords: Vec<_> = visible.iter().map(|(c, _, _)| *c).collect();
                        let (vis_gpu, vis_ready, vis_pending) = self.tile_manager.as_ref().unwrap().stats_for_visible(&visible_coords);
                        let (total_gpu, total_mem, _total_pnd) = self.tile_manager.as_ref().unwrap().tiles_and_pending();
                        
                        let debug_text = format!(
                            "VIS: {} (GPU:{} RDY:{} PND:{}) | ALL: (GPU:{} MEM:{}) | SCALE: {:.3}", 
                            visible.len(), vis_gpu, vis_ready, vis_pending, total_gpu, total_mem, effective_scale
                        );
                        ui.painter().text(
                            screen_rect.right_bottom() - egui::vec2(10.0, 10.0),
                            egui::Align2::RIGHT_BOTTOM,
                            debug_text,
                            egui::FontId::monospace(14.0),
                            Color32::from_rgb(0, 255, 0),
                        );
                    }

                    // ANTI-STALL LOGIC:
                    // If we uploaded tiles this frame, OR if there are more ready to upload in CPU cache,
                    // request another repaint immediately to keep the pipeline moving.
                    let visible_coords: Vec<_> = visible.iter().map(|(c, _, _)| *c).collect();
                    let has_more_ready = self.tile_manager.as_ref().unwrap().has_ready_to_upload(&visible_coords);
                    if newly_uploaded > 0 || has_more_ready {
                        ui.ctx().request_repaint();
                    }
                }
            } else if let Some(texture) = self.texture_cache.get(self.current_index).cloned() {
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
                // Use original image dimensions if known (Tiled previews are smaller than the real image)
                let img_size = if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
                    Vec2::new(w as f32, h as f32)
                } else {
                    texture.size_vec2()
                };

                if canvas_resp.dragged() {
                    self.pan_offset += canvas_resp.drag_delta();
                    // Bumping generation here ensures that if we zoom into tiled mode later,
                    // or if multiple levels of tiled loaders exist, the priority is reset.
                    self.generation = self.generation.wrapping_add(1);
                    self.loader.set_generation(self.generation);
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

                // Rotation logic
                let rotation = self.current_rotation;
                let needs_swap = rotation % 2 != 0;
                let angle = rotation as f32 * (std::f32::consts::PI / 2.0);

                // Compute current display rect with swapped dimensions if needed for proper fit-to-window scaling
                let rotated_img_size = if needs_swap { Vec2::new(img_size.y, img_size.x) } else { img_size };
                let dest = self.compute_display_rect(rotated_img_size, screen_rect);
                let final_dest = Rect::from_center_size(
                    dest.center() + offset,
                    dest.size() * scale
                );

                // The painter transform handles the visual rotation.
                // We draw the un-rotated texture into an "un-rotated" rect.
                let unrotated_final_size = if needs_swap { Vec2::new(final_dest.height(), final_dest.width()) } else { final_dest.size() };
                let unrotated_final_dest = Rect::from_center_size(final_dest.center(), unrotated_final_size);

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

                                let elapsed = self.transition_start.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
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

                                let mut mesh = egui::Mesh::with_texture(texture.id());
                                mesh.add_rect_with_uv(unrotated_final_dest, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE.linear_multiply(alpha));
                                if rotation != 0 {
                                    let rot = egui::emath::Rot2::from_angle(angle);
                                    let pivot = final_dest.center();
                                    for v in &mut mesh.vertices {
                                        v.pos = pivot + rot * (v.pos - pivot);
                                    }
                                }
                                ui.painter().with_clip_rect(new_clip).add(egui::Shape::mesh(mesh));

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
                            let elapsed = self.transition_start.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
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

                            let mut mesh = mesh;
                            if rotation != 0 {
                                let rot = egui::emath::Rot2::from_angle(angle);
                                let pivot = dest.center();
                                for v in &mut mesh.vertices {
                                    v.pos = pivot + rot * (v.pos - pivot);
                                }
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

                        let elapsed = self.transition_start.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
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

                    let mut mesh = egui::Mesh::with_texture(texture.id());
                    mesh.add_rect_with_uv(unrotated_final_dest, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE.linear_multiply(alpha));
                    if rotation != 0 {
                        let rot = egui::emath::Rot2::from_angle(angle);
                        let pivot = final_dest.center();
                        for v in &mut mesh.vertices {
                            v.pos = pivot + rot * (v.pos - pivot);
                        }
                    }
                    ui.painter().add(egui::Shape::mesh(mesh));
                }
            }
            
            // 3. GLOBAL HUD OVERLAY (OSD)
            // Drawn outside the texture-success branch to ensure persistent display 
            // during refinement, transitions, or slow tile loading.
            if self.settings.show_osd {
                let res = if let Some(r) = self.current_image_res { r } else { (0, 0) };
                let img_size = Vec2::new(res.0 as f32, res.1 as f32);
                let rotation = self.current_rotation;
                let needs_swap = rotation % 2 != 0;
                let rotated_img_size = if needs_swap { Vec2::new(img_size.y, img_size.x) } else { img_size };

                let effective_scale = self.calculate_effective_scale(rotated_img_size, screen_rect);
                let zoom_pct = (effective_scale * self.cached_pixels_per_point * 100.0).round() as u32;
                
                // Determine resolution and mode tag
                let mut res_w = 0;
                let mut res_h = 0;
                let mut mode_tag = "STATIC";

                if let Some(tm) = &self.tile_manager {
                    res_w = tm.full_width;
                    res_h = tm.full_height;
                    mode_tag = "TILED";
                } else if let Some((w, h)) = self.current_image_res {
                    res_w = w;
                    res_h = h;
                    
                    // Pre-detect if this will become a TILED image based on threshold
                    let threshold = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
                    if w as u64 * h as u64 > threshold {
                        mode_tag = "TILED";
                    }
                }

                if res_w > 0 {
                    let current_state = crate::ui::osd::OsdState {
                        index: self.current_index,
                        total: self.image_files.len(),
                        zoom_pct,
                        res: (res_w, res_h),
                        mode: mode_tag.to_string(),
                        current_track: self.audio.get_current_track(),
                        metadata: self.audio.get_metadata(),
                        current_cue_track: self.audio.get_current_cue_track(),
                        current_pos_ms: self.audio.get_pos_ms(),
                        total_duration_ms: self.audio.get_duration_ms(),
                        cue_markers: self.audio.get_cue_markers(),
                    };

                    let fname = self.image_files[self.current_index]
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy();
                    
                    self.osd.render(
                        ui,
                        screen_rect,
                        &current_state,
                        &fname,
                        &self.cached_palette,
                        &self.last_save_error,
                    );
                }

                if res_w == 0 {
                    // While loading/parsing, show a minimal status
                    self.osd.render_loading_hint(ui, screen_rect, &self.cached_palette);
                }

                // Hint when settings hidden
                if !self.show_settings {
                    ui.painter().text(
                        screen_rect.right_bottom() + Vec2::new(-12.0, -12.0),
                        Align2::RIGHT_BOTTOM,
                        t!("hint.keyboard").to_string(),
                        FontId::proportional(13.0),
                        self.cached_palette.osd_hint,
                    );
                }
            }

        });
    }

    /// Calculate current absolute display scale relative to image pixels (logical scale).
    fn calculate_effective_scale(&self, img_size: Vec2, screen_rect: Rect) -> f32 {
        match self.settings.scale_mode {
            ScaleMode::FitToWindow => {
                if img_size.x > 0.1 && img_size.y > 0.1 {
                    (screen_rect.width() / img_size.x)
                        .min(screen_rect.height() / img_size.y)
                        * self.zoom_factor
                } else {
                    self.zoom_factor
                }
            }
            ScaleMode::OriginalSize => self.zoom_factor / self.cached_pixels_per_point,
        }
    }

    /// Compute the display rect for an image texture within the screen.
    fn compute_display_rect(&self, img_size: Vec2, screen_rect: Rect) -> Rect {
        let scale = self.calculate_effective_scale(img_size, screen_rect);
        match self.settings.scale_mode {
            ScaleMode::FitToWindow => {
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
    
    /// Rotate the image while keeping the current screen center point fixed on the same image coordinate.
    fn apply_rotation_with_tracking(&mut self, clockwise: bool, ctx: &Context) {
        if self.image_files.is_empty() { return; }
        
        // 1. Get original image resolution
        let res = if let Some(r) = self.current_image_res { r } else { return; };
        let img_size = Vec2::new(res.0 as f32, res.1 as f32);
        let screen_rect = ctx.input(|i| i.content_rect());
        
        // 2. Calculate current scale
        let old_rotation = self.current_rotation;
        let old_needs_swap = old_rotation % 2 != 0;
        let old_rotated_size = if old_needs_swap { Vec2::new(img_size.y, img_size.x) } else { img_size };
        let old_scale = self.calculate_effective_scale(old_rotated_size, screen_rect);

        // 3. Update rotation state
        if clockwise {
            self.current_rotation = (self.current_rotation + 1) % 4;
        } else {
            self.current_rotation = (self.current_rotation + 3) % 4;
        }

        // 4. Calculate new scale (FitToWindow scale might change due to aspect ratio swap)
        let new_rotation = self.current_rotation;
        let new_needs_swap = new_rotation % 2 != 0;
        let new_rotated_size = if new_needs_swap { Vec2::new(img_size.y, img_size.x) } else { img_size };

        let new_scale = self.calculate_effective_scale(new_rotated_size, screen_rect);

        // 5. Transform pan_offset to maintain center alignment.
        // Rotation around image center maps (x, y) to (-y, x) for CW 90.
        // We also compensate for scale changes to keep the visual point fixed.
        let p = self.pan_offset;
        if clockwise {
            // Clockwise: (x, y) -> (-y, x)
            self.pan_offset = Vec2::new(-p.y, p.x);
        } else {
            // Counter-clockwise: (x, y) -> (y, -x)
            self.pan_offset = Vec2::new(p.y, -p.x);
        }

        if old_scale > 0.0001 {
            self.pan_offset *= new_scale / old_scale;
        }

        // Invalidate tiled caches to re-request tiles in new orientation
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        if let Some(tm) = &mut self.tile_manager {
            tm.generation = self.generation;
            tm.pending_tiles.clear();
        }
    }
}

impl eframe::App for ImageViewerApp {
    fn on_exit(&mut self) {
        if self.settings.resume_last_image && !self.image_files.is_empty() {
            self.settings.last_viewed_image = Some(self.image_files[self.current_index].clone());
            self.queue_save();
        }

        // Force-terminate BEFORE eframe tries to tear down GPU resources.
        // This avoids a DLL loader lock deadlock on Windows where:
        //   - rayon worker threads hold the loader lock during TLS cleanup
        //   - WIC's CCodecFactory destructor calls MFShutdown which waits for internal timer threads
        //   - main thread's D3D12 adapter drop calls FreeLibrary which needs the loader lock
        // Settings are already persisted above, so this is safe.
        #[cfg(target_os = "windows")]
        std::process::exit(0);
    }

    /// Background logic: scanning, loading, auto-switch, keyboard, timers.
    /// Called before each ui() call (and also when hidden but repaint requested).
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Global mouse activity detection to wake up Music HUD
        if ctx.input(|i| i.pointer.delta().length_sq() > 0.0) {
            self.music_hud_last_activity = Instant::now();
        }


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

        // ── Drag-and-Drop handling (cross-platform via egui/winit) ───────
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(dropped_file) = dropped.into_iter().next() {
            if let Some(path) = dropped_file.path {
                // Guard: don't re-trigger if we're already scanning from a previous drop
                if !self.scanning {
                    if path.is_dir() {
                        // Dropped a directory — scan it (non-recursive to avoid surprises)
                        log::info!("Drop: opening directory {:?}", path);
                        self.settings.recursive = false;
                        self.load_directory(path);
                        self.queue_save();
                    } else if path.is_file() {
                        // Dropped a single file — check if it's a supported format
                        let is_supported = path.extension()
                            .map(|ext| crate::scanner::is_supported_extension(ext))
                            .unwrap_or(false);

                        if is_supported {
                            log::info!("Drop: opening file {:?}", path);
                            if let Some(parent) = path.parent() {
                                self.initial_image = Some(path.clone());
                                self.settings.auto_switch = false;
                                self.load_directory(parent.to_path_buf());
                                self.queue_save();
                            }
                        } else {
                            log::warn!("Drop: ignored unsupported file format {:?}", path);
                        }
                    }
                    ctx.request_repaint();
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
            self.osd.invalidate(); // Invalidate HUD cache to force total redraw
            ctx.request_repaint();
        }

        // Automatic theme refresh (for System theme trailing detection)
        // Only reconstructs palette when theme actually changes (avoids per-frame allocation)
        if let Some(new_palette) = self.settings.theme.resolve_if_changed(&mut self.theme_cache) {
            self.cached_palette = new_palette;
            // Always refresh visuals if resolve_if_changed returns Some, to ensure
            // all style properties (including those not in is_dark) are synchronized.
            setup_visuals(ctx, &self.settings, &self.cached_palette);
        }

        let ppp = ctx.pixels_per_point();
        if (ppp - self.cached_pixels_per_point).abs() > 0.001 {
            self.cached_pixels_per_point = ppp;
            setup_visuals(ctx, &self.settings, &self.cached_palette);
        }

        // Poll persistence errors from the saver thread
        while let Ok(err) = self.save_error_rx.try_recv() {
            log::error!("Settings persistence error: {}", err);
            self.last_save_error = Some((err, Instant::now()));
        }

        // Clear persistence error after 5 seconds
        if let Some((_, start)) = self.last_save_error {
            if start.elapsed().as_secs() >= 5 {
                self.last_save_error = None;
            }
        }

        self.process_scan_results();
        self.process_music_scan_results();
        self.process_loaded_images(ctx);

        // Check if the audio thread detected a hardware stall (e.g. WASAPI exclusive
        // mode preemption) and needs a full restart — same path as toggling the checkbox.
        if self.settings.play_music && self.audio.take_needs_restart() {
            log::warn!("[UI] Audio stall detected by watchdog, triggering full restart");
            self.force_restart_audio();
        }

        self.check_auto_switch();
        self.handle_keyboard(ctx);

        // Sync currently playing track path and CUE track for persistence
        if self.settings.play_music {
            let mut changed = false;
            if let Some(current_path) = self.audio.get_current_track_path() {
                if self.settings.last_music_file.as_ref() != Some(&current_path) {
                    self.settings.last_music_file = Some(current_path);
                    changed = true;
                }
            }
            let cue_idx = self.audio.get_current_cue_track();
            if self.settings.last_music_cue_track != cue_idx {
                self.settings.last_music_cue_track = cue_idx;
                changed = true;
            }
            
            if changed {
                self.queue_save();
            }
        }

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

        if self.is_printing.load(std::sync::atomic::Ordering::Relaxed) {
            egui::Window::new(if cfg!(not(target_os = "windows")) { t!("print.title_pdf").to_string() } else { t!("print.title").to_string() })
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t!("print.processing").to_string());
                    });
                });
            
            if let Some(rx) = &self.print_status_rx {
                while let Ok(msg) = rx.try_recv() {
                    if let Some(m) = msg {
                        self.status_message = t!("print.failed", err = m).to_string();
                    }
                }
            }
        } else if let Some(rx) = self.print_status_rx.take() {
            while let Ok(msg) = rx.try_recv() {
                if let Some(m) = msg {
                    self.status_message = t!("print.failed", err = m).to_string();
                }
            }
        }

        // Settings panel overlay (hidden when file assoc dialog is modal)
        #[cfg(target_os = "windows")]
        let skip_settings = self.show_file_assoc_dialog;
        #[cfg(not(target_os = "windows"))]
        let skip_settings = false;

        // Settings panel & Dialogs
        if self.show_settings && !skip_settings {
            self.draw_settings_panel(&ctx);
        } else {
            self.last_show_settings = false;
        }
        self.draw_wallpaper_dialog(&ctx);
        #[cfg(target_os = "windows")]
        self.draw_file_assoc_dialog(&ctx);
        self.draw_goto_dialog(&ctx);
        ui_dialogs::exif::draw(self, &ctx);
        ui_dialogs::xmp::draw(self, &ctx);

        // ── Music HUD (Foreground Layer) ─────────────────────────────────
        self.draw_music_hud_foreground(&ctx);
    }
}

pub(crate) fn extract_exif(path: &std::path::Path) -> Option<Vec<(String, String)>> {
    use std::fs::File;
    use std::io::BufReader;
    
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(&file);
    let exifreader = exif::Reader::new();
    let exif = exifreader.read_from_container(&mut reader).ok()?;

    let mut result = Vec::new();
    for f in exif.fields() {
        let tag = format!("{}", f.tag);
        let val = format!("{}", f.display_value().with_unit(&exif));
        result.push((tag, val));
    }
    
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

pub(crate) fn extract_xmp(path: &std::path::Path) -> Option<(Vec<(String, String)>, String)> {
    use xmpkit::XmpFile;
    use quick_xml::reader::Reader;
    use quick_xml::events::Event;
    use std::collections::BTreeMap;
    
    let mut file = XmpFile::new();
    if file.open(path.to_string_lossy().as_ref()).is_err() {
        return None;
    }
    
    let meta = file.get_xmp()?;
    let xml_str = match meta.serialize() {
        Ok(s) => s,
        Err(_) => return None,
    };
    
    let mut reader = Reader::from_str(&xml_str);
    reader.config_mut().trim_text(true);
    
    let mut result_map = BTreeMap::new();
    let mut buf = Vec::new();
    let mut stack = Vec::new();
    
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                
                // Skip structural RDF tags to keep paths clean
                let is_structural = name.starts_with("rdf:") || name == "x:xmpmeta";
                if !is_structural {
                    stack.push(name.clone());
                }
                
                // Process attributes (e.g., x:xmptk or compact RDF properties)
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                    if key.starts_with("xmlns:") || key == "rdf:about" {
                        continue;
                    }
                    let val = attr.unescape_value().unwrap_or_default().to_string();
                    if !val.is_empty() {
                        let path = if stack.is_empty() { key } else { format!("{}.{}", stack.join("."), key) };
                        result_map.insert(path, val);
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                // Self-closing tag: process attributes but don't stay on stack
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let is_structural = name.starts_with("rdf:") || name == "x:xmpmeta";
                
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                    if key.starts_with("xmlns:") || key == "rdf:about" {
                        continue;
                    }
                    let val = attr.unescape_value().unwrap_or_default().to_string();
                    if !val.is_empty() {
                        let path = if is_structural { key } else { format!("{}.{}", name, key) };
                        result_map.insert(path, val);
                    }
                }
            }
            Ok(Event::Text(e)) => {
                let val = reader.decoder().decode(e.as_ref()).unwrap_or_default().to_string();
                if !val.is_empty() && !stack.is_empty() {
                    let path = stack.join(".");
                    result_map.insert(path, val);
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if !name.starts_with("rdf:") && name != "x:xmpmeta" {
                    stack.pop();
                }
            }
            Ok(Event::Eof) => break,
            _ => (),
        }
        buf.clear();
    }
    
    let mut final_data = Vec::new();
    for (k, v) in result_map {
        // Final cleanup of common prefixes to look like exiftool
        let mut clean_k = k.replace("rdf:", "");
        if clean_k.starts_with("x:xmptk") {
            clean_k = "XMP Toolkit".to_string();
        }
        final_data.push((clean_k, v));
    }

    if final_data.is_empty() {
        None
    } else {
        Some((final_data, xml_str))
    }
}

