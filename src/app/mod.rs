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

// ── Submodules ──────────────────────────────────────────────────────────────
pub(crate) mod image_management;
pub(crate) mod input;
pub(crate) mod lifecycle;
pub(crate) mod media;
pub(crate) mod rendering;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Context, Pos2, Vec2};

use crate::audio::AudioPlayer;
use crate::ipc::IpcMessage;
use crate::loader::{ImageLoader, TextureCache};
use crate::scanner;
pub(crate) use crate::settings::{ScaleMode, Settings, TransitionStyle};
pub(crate) use crate::theme::AppTheme;
use crate::theme::{SystemThemeCache, ThemePalette};
use crate::tile_cache::TileManager;
use crate::ui::dialogs::modal_state::ActiveModal;
use crate::ui::utils::setup_visuals;
use rust_i18n::t;

// -- Preload configuration --
// Maximum number of images to preload in each direction.
pub(crate) const MAX_PRELOAD_FORWARD: usize = 5;
pub(crate) const MAX_PRELOAD_BACKWARD: usize = 3;
// Texture cache must hold: current + forward + backward + buffer for transitions
pub(crate) const CACHE_SIZE: usize = MAX_PRELOAD_FORWARD + MAX_PRELOAD_BACKWARD + 3;

/// Compute preload byte budgets based on total system RAM.
/// Forward budget = total_ram / 32, backward = total_ram / 64, both clamped.
pub(crate) fn compute_preload_budgets() -> (u64, u64) {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let total = sys.total_memory(); // bytes

    let forward = (total / 32).clamp(64 * 1024 * 1024, 512 * 1024 * 1024);
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
pub(crate) struct AnimationPlayback {
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
            Self::Low => 256,    // Basic coverage
            Self::Medium => 448, // Retina/4K coverage
            Self::High => 1024,  // Performance/Gigapixel coverage
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

pub enum FileOpResult {
    Delete(PathBuf, usize, Result<(), String>),
    Exif(PathBuf, Option<Vec<(String, String)>>),
    Xmp(PathBuf, Option<(Vec<(String, String)>, String)>),
    Wallpaper(Option<String>),
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
    /// True once the very first directory scan has produced at least one image.
    pub(crate) images_ever_loaded: bool,
    pub(crate) status_message: String,
    pub(crate) error_message: Option<String>,
    pub(crate) is_font_error: bool,
    /// Incremented each time a modal dialog is opened.
    /// Included in each dialog's egui Window Id so that egui has no position
    /// memory from a previous opening — the dialog always starts centered.
    pub(crate) modal_generation: u32,

    // Pending viewport commands (set during input processing for deferred apply)
    pub(crate) pending_fullscreen: Option<bool>,

    // Cached system font families
    pub(crate) font_families: Vec<String>,
    pub(crate) temp_font_size: Option<f32>,

    // Cached state
    pub(crate) generation: u64,
    pub(crate) cached_music_count: Option<usize>,
    pub(crate) cached_pixels_per_point: f32,

    // Active modal dialog — only one can be open at a time.
    // All per-dialog state lives inside the enum variant; setting this to None
    // automatically drops and cleans up the dialog's temporary data.
    pub(crate) active_modal: Option<ActiveModal>,

    // Async music scanning
    pub(crate) music_scan_rx: Option<Receiver<Vec<PathBuf>>>,
    pub(crate) scanning_music: bool,
    pub(crate) music_scan_cancel: Option<Arc<AtomicBool>>,
    pub(crate) music_scan_path: Option<PathBuf>,
    pub(crate) scan_rx: Option<Receiver<scanner::ScanMessage>>,
    pub(crate) scan_cancel: Option<Arc<AtomicBool>>,

    // Current image resolution (used by wallpaper dialog and OSD)
    pub(crate) current_image_res: Option<(u32, u32)>,

    // Transition state
    pub(crate) prev_texture: Option<egui::TextureHandle>,
    pub(crate) transition_start: Option<Instant>,
    pub(crate) is_next: bool,
    pub(crate) active_transition: TransitionStyle,

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

    // Async file operations (deletion, etc.)
    pub(crate) file_op_rx: Receiver<FileOpResult>,
    pub(crate) file_op_tx: Sender<FileOpResult>,

    // Debounce for mouse wheel navigation
    pub(crate) last_mouse_wheel_nav: f64,

    // Settings persistence channel
    pub(crate) save_tx: Sender<Settings>,
    pub(crate) save_error_rx: Receiver<String>,
    pub(crate) last_save_error: Option<(String, Instant)>,
    pub(crate) saver_handle: Option<std::thread::JoinHandle<()>>,

    // Preload byte budgets (computed at startup from system RAM)
    pub(crate) preload_budget_forward: u64,
    pub(crate) preload_budget_backward: u64,

    // Custom right-click context menu (bypasses egui's context_menu which
    // cannot re-open on consecutive right-clicks)
    pub(crate) context_menu_pos: Option<Pos2>,
    /// Current view rotation in steps of 90 degrees clockwise (0-3).
    pub(crate) current_rotation: i32,

    // Adaptive tile upload quota based on hardware and current frame performance
    pub(crate) tile_upload_quota: usize,

    // Audio device caching
    pub(crate) cached_audio_devices: Vec<String>,

    // Music HUD drag offset (user-adjustable position relative to default bottom-center)
    pub(crate) music_hud_drag_offset: Vec2,
}

/// Holds animation frame data waiting to be uploaded to GPU across multiple frames.
pub(crate) struct PendingAnimUpload {
    image_index: usize,
    frames: Vec<crate::loader::AnimationFrame>,
    textures: Vec<egui::TextureHandle>,
    delays: Vec<std::time::Duration>,
    next_frame: usize,
}

impl eframe::App for ImageViewerApp {
    fn on_exit(&mut self) {
        if self.settings.resume_last_image && !self.image_files.is_empty() {
            self.settings.last_viewed_image = Some(self.image_files[self.current_index].clone());
        }
        // Shut down the async saver thread first: dropping the sender closes the
        // channel, causing the saver's `recv()` loop to exit after finishing any
        // in-progress write. This eliminates the race between the saver and our
        // synchronous save below.
        let (dummy_tx, _) = crossbeam_channel::unbounded::<Settings>();
        let old_tx = std::mem::replace(&mut self.save_tx, dummy_tx);
        drop(old_tx);

        // Wait for the saver thread to finish any in-progress I/O
        if let Some(handle) = self.saver_handle.take() {
            if let Err(e) = handle.join() {
                log::error!("[on_exit] Saver thread panicked: {:?}", e);
            }
        }

        if let Err(e) = self.settings.save() {
            log::error!("[on_exit] Failed to save settings: {}", e);
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
                        let same_dir = self
                            .settings
                            .last_image_dir
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
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        crate::ipc::force_foreground();
                    }
                }
                IpcMessage::OpenImageNoRecursive(path) => {
                    log::info!("IPC: open image (no-recursive) {:?}", path);
                    if let Some(parent) = path.parent() {
                        let same_dir = self
                            .settings
                            .last_image_dir
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
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        crate::ipc::force_foreground();
                    }
                }
                IpcMessage::Focus => {
                    log::info!("IPC received empty ping, requesting window focus");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
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
                        let is_supported = path
                            .extension()
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
        if let Some(new_palette) = self
            .settings
            .theme
            .resolve_if_changed(&mut self.theme_cache)
        {
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
        self.process_file_op_results();

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

                let cue_idx = self.audio.get_current_cue_track();
                if self.settings.last_music_cue_track != cue_idx {
                    self.settings.last_music_cue_track = cue_idx;
                    changed = true;
                }
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
            egui::Window::new(if cfg!(not(target_os = "windows")) {
                t!("print.title_pdf").to_string()
            } else {
                t!("print.title").to_string()
            })
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

        // Settings panel overlay.
        // Suppressed while a modal dialog is open: the modal dialog's backdrop
        // only dims visually (Order::Background); to achieve true modality we
        // must prevent the settings panel from being rendered (and thus from
        // receiving input) while a dialog is on screen.
        let modal_open = self.active_modal.is_some();
        if self.show_settings && !modal_open {
            self.draw_settings_panel(&ctx);
        } else if !self.show_settings {
            self.last_show_settings = false;
        }

        // Detect modal transitions: None → Some means a new dialog just opened.
        // Incrementing modal_generation makes the egui::Window Id unique for this
        // opening — egui has no position memory from previous openings, so the
        // dialog always appears at the calculated center position.
        {
            let id = egui::Id::new(crate::ui::dialogs::modal_state::ID_PREV_HAD_MODAL);
            let had_modal = ctx.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
            let has_modal = self.active_modal.is_some();
            if has_modal && !had_modal {
                self.modal_generation = self.modal_generation.wrapping_add(1);
            }
            ctx.data_mut(|d| d.insert_temp(id, has_modal));
        }

        // Dispatch the single active modal dialog (MovableModal handles the overlay)
        self.dispatch_active_modal(&ctx);

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
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    use std::collections::BTreeMap;
    use xmpkit::XmpFile;

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
                        let path = if stack.is_empty() {
                            key
                        } else {
                            format!("{}.{}", stack.join("."), key)
                        };
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
                        let path = if is_structural {
                            key
                        } else {
                            format!("{}.{}", name, key)
                        };
                        result_map.insert(path, val);
                    }
                }
            }
            Ok(Event::Text(e)) => {
                let val = reader
                    .decoder()
                    .decode(e.as_ref())
                    .unwrap_or_default()
                    .to_string();
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
