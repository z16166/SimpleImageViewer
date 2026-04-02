use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use egui::{
    Align2, Color32, ColorImage, Context, FontId, Frame, Key, Pos2, Rect, RichText,
    Sense, TextureOptions, Vec2,
};

use crate::audio::{AudioPlayer, collect_music_files};
use crate::loader::{ImageLoader, TextureCache};
use crate::scanner;
use crate::settings::{ScaleMode, Settings};

const PRELOAD_AHEAD: usize = 2;
const PRELOAD_BEHIND: usize = 1;
const CACHE_SIZE: usize = 5; // 1 current + PRELOAD_AHEAD + PRELOAD_BEHIND + 1 buffer

// Accent colors for the UI
const BG_DARK: Color32 = Color32::from_rgb(18, 18, 24);
const PANEL_BG: Color32 = Color32::from_rgb(28, 28, 38);
const ACCENT: Color32 = Color32::from_rgb(108, 92, 231);
const ACCENT2: Color32 = Color32::from_rgb(0, 199, 190);
const TEXT_MUTED: Color32 = Color32::from_rgb(130, 130, 155);


pub struct ImageViewerApp {
    settings: Settings,

    // File list
    image_files: Vec<PathBuf>,
    current_index: usize,

    // Channel receiving scanned file list
    scan_rx: Option<Receiver<Vec<PathBuf>>>,
    scanning: bool,

    // Image loading
    loader: ImageLoader,
    texture_cache: TextureCache,

    // Pan/drag state (used in non-fullscreen 1:1 mode)
    pan_offset: Vec2,

    // Manual zoom factor (1.0 = 100%); applied on top of any fit-to-screen scale
    zoom_factor: f32,

    // Auto-switch timer
    last_switch_time: Instant,

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
    cached_music_count: Option<usize>,
    cached_pixels_per_point: f32,

    // EXIF dialog state
    show_exif_window: bool,
    cached_exif_text: Option<String>,

    // Goto dialog state
    show_goto: bool,
    goto_input: String,
    goto_needs_focus: bool,
}

impl ImageViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        if settings.fullscreen {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        setup_visuals(&cc.egui_ctx, &settings);
        setup_fonts(&cc.egui_ctx, &settings);

        let mut app = Self {
            settings,
            image_files: Vec::new(),
            current_index: 0,
            scan_rx: None,
            scanning: false,
            loader: ImageLoader::new(),
            texture_cache: TextureCache::new(CACHE_SIZE),
            pan_offset: Vec2::ZERO,
            zoom_factor: 1.0,
            last_switch_time: Instant::now(),
            audio: AudioPlayer::new(),
            show_settings: true,
            status_message: "Open a directory to start viewing images".to_string(),
            error_message: None,
            pending_fullscreen: None,
            font_families: get_system_font_families(),
            temp_font_size: None,
            cached_music_count: None,
            cached_pixels_per_point: 1.0,
            show_exif_window: false,
            cached_exif_text: None,
            show_goto: false,
            goto_input: String::new(),
            goto_needs_focus: false,
        };

        // Restore last session state
        if let Some(dir) = app.settings.last_image_dir.clone() {
            app.load_directory(dir);
        }
        if let Some(p) = &app.settings.music_path {
            app.cached_music_count = Some(collect_music_files(p).len());
        }
        if app.settings.play_music {
            app.restart_audio_if_enabled();
        }

        app
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
        let new_index = new_index.min(self.image_files.len() - 1);
        let prev = self.current_index;
        self.current_index = new_index;
        self.pan_offset = Vec2::ZERO;
        self.zoom_factor = 1.0;
        self.last_switch_time = Instant::now();
        self.error_message = None;
        self.schedule_preloads(new_index >= prev);
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
                self.loader.request_load(idx, path);
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

            if self.settings.resume_last_image {
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
            match load_result.result {
                Ok(decoded) => {
                    let color_image = ColorImage::from_rgba_unmultiplied(
                        [decoded.width as usize, decoded.height as usize],
                        &decoded.pixels,
                    );
                    let name = format!("img_{}", load_result.index);
                    let handle =
                        ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                    self.texture_cache
                        .insert(load_result.index, handle, self.current_index);
                }
                Err(e) => {
                    log::warn!(
                        "Failed to load image at index {}: {e}",
                        load_result.index
                    );
                    if load_result.index == self.current_index {
                        self.error_message =
                            Some(format!("Failed to load image: {e}"));
                    }
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
        #[allow(unused_mut)]
        let mut do_quit = false;
 
        ctx.input(|i| {
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
            // Mouse wheel zoom
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
            // Quit shortcut: Cmd+Q on macOS, Ctrl+Q on Linux.
            // On Windows, Alt+F4 is standard and is handled by the OS — no code needed.
            #[cfg(not(target_os = "windows"))]
            if i.modifiers.command && i.key_pressed(Key::Q) {
                do_quit = true;
            }
        });

        if nav_next { self.navigate_next(); }
        if nav_prev { self.navigate_prev(); }
        if nav_first { self.navigate_first(); }
        if nav_last { self.navigate_last(); }
        if toggle_settings {
            self.show_settings = !self.show_settings;
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
        if scroll_delta > 0.0 {
            self.zoom_factor = (self.zoom_factor * 1.25).min(20.0);
        } else if scroll_delta < 0.0 {
            self.zoom_factor = (self.zoom_factor / 1.25).max(0.05);
        }
        if toggle_fullscreen {
            self.settings.fullscreen = !self.settings.fullscreen;
            self.pending_fullscreen = Some(self.settings.fullscreen);
            self.settings.save();
        }
        if toggle_scale_mode {
            self.settings.scale_mode = self.settings.scale_mode.toggled();
            self.zoom_factor = 1.0;
            self.pan_offset = Vec2::ZERO;
            self.settings.save();
        }
        if toggle_auto_switch {
            self.settings.auto_switch = !self.settings.auto_switch;
            if self.settings.auto_switch {
                self.last_switch_time = Instant::now();
            }
            self.settings.save();
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
        if !self.settings.auto_switch || self.image_files.is_empty() {
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
            self.cached_music_count = Some(collect_music_files(&path).len());
            self.restart_audio_if_enabled();
        }
    }

    fn open_music_dir_dialog(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.settings.music_path = Some(dir.clone());
            self.cached_music_count = Some(collect_music_files(&dir).len());
            self.restart_audio_if_enabled();
        }
    }

    fn restart_audio_if_enabled(&mut self) {
        self.audio.stop();
        if self.settings.play_music {
            if let Some(ref path) = self.settings.music_path.clone() {
                let files = collect_music_files(path);
                if !files.is_empty() {
                    self.audio.start(files);
                    self.audio.set_volume(self.settings.volume);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // UI: Settings panel
    // ------------------------------------------------------------------

    fn draw_settings_panel(&mut self, ctx: &Context) {
        let mut open_dir = false;
        let mut open_music_file = false;
        let mut open_music_dir = false;
        let mut start_viewing = false;
        let mut fullscreen_changed = false;
        let mut music_enabled_changed = false;
        let mut do_quit = false;

        egui::Window::new("⚙  Settings")
            .anchor(Align2::LEFT_TOP, [12.0, 12.0])
            .resizable(false)
            .collapsible(false)
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
                if old_recursive != self.settings.recursive {
                    if let Some(dir) = self.settings.last_image_dir.clone() {
                        self.load_directory(dir);
                    }
                    self.settings.save();
                }

                if ui.checkbox(&mut self.settings.preload, "Enable image preloading").changed() {
                    self.settings.save();
                }

                if ui.checkbox(&mut self.settings.resume_last_image, "Resume from last viewed image").changed() {
                    self.settings.save();
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
                    self.settings.save();
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Press Z to toggle scale mode")
                        .color(TEXT_MUTED)
                        .small(),
                );

                ui.add_space(6.0);
                if ui.checkbox(&mut self.settings.show_osd, "Show OSD info (texts overlaid on image)").changed() {
                    self.settings.save();
                }

                ui.add_space(8.0);
                }); // End Left Column
                
                cols[1].vertical(|ui| {
                // ── Slideshow ────────────────────────────────────────────
                ui.label(RichText::new("Slideshow").color(ACCENT2).strong());
                ui.add_space(2.0);

                let old_auto_switch = self.settings.auto_switch;
                ui.checkbox(&mut self.settings.auto_switch, "Auto-advance to next image");
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
                    self.settings.save();
                }

                ui.add_space(8.0);

                // ── Music ──────────────────────────────────────────────────
                ui.label(RichText::new("Background Music").color(ACCENT2).strong());
                ui.add_space(2.0);

                let old_play_music = self.settings.play_music;
                ui.checkbox(&mut self.settings.play_music, "Play background music (MP3/FLAC/OGG/WAV/AAC/M4A)");
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
                        let n = self.cached_music_count.unwrap_or(0);
                        if n == 0 {
                            ui.label(
                                RichText::new("⚠ No supported audio files found (MP3/FLAC/OGG/WAV/AAC/M4A)")
                                    .color(Color32::from_rgb(255, 180, 60))
                                    .small(),
                            );
                        } else {
                            ui.label(
                                RichText::new(format!("♪ {n} file(s) ready"))
                                    .color(ACCENT2)
                                    .small(),
                            );
                        }
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
                            self.settings.save();
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
                        self.settings.save();
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
                        self.settings.save();
                    }
                });

                ui.add_space(8.0);
                }); // End of Right Column
                }); // End of ui.columns

                ui.separator();
                ui.add_space(6.0);

                // ── Status & actions ───────────────────────────────────────
                if !self.image_files.is_empty() {
                    let fname = self.image_files[self.current_index]
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy();
                    ui.label(
                        RichText::new(format!(
                            "{}/{} — {}",
                            self.current_index + 1,
                            self.image_files.len(),
                            fname
                        ))
                        .color(TEXT_MUTED)
                        .small(),
                    );
                    ui.add_space(4.0);
                    if styled_button(ui, "▶  Start Viewing").clicked() {
                        start_viewing = true;
                    }
                } else {
                    ui.label(
                        RichText::new(&self.status_message)
                            .color(TEXT_MUTED)
                            .small(),
                    );
                }

                ui.add_space(6.0);
                #[cfg(target_os = "macos")]
                const QUIT_HINT: &str =
                    "Press  Esc / F1  to toggle this panel  \u{2502}  Cmd+Q to quit";
                #[cfg(target_os = "linux")]
                const QUIT_HINT: &str =
                    "Press  Esc / F1  to toggle this panel  \u{2502}  Ctrl+Q to quit";
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                const QUIT_HINT: &str =
                    "Press  Esc / F1  to toggle this panel  \u{2502}  Alt+F4 to quit";
                ui.label(RichText::new(QUIT_HINT).color(TEXT_MUTED).small());

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
                // Exit button — always visible at bottom of settings panel
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
            });

        // Deferred actions (avoid borrow issues with closures)
        if open_dir {
            self.open_directory_dialog();
            self.settings.save(); // saves last_image_dir
        }
        if open_music_file {
            self.open_music_file_dialog();
            self.settings.save(); // saves music_path
        }
        if open_music_dir {
            self.open_music_dir_dialog();
            self.settings.save(); // saves music_path
        }
        if start_viewing {
            self.show_settings = false;
        }
        if fullscreen_changed {
            self.pending_fullscreen = Some(self.settings.fullscreen);
            self.settings.save();
        }
        if music_enabled_changed {
            self.restart_audio_if_enabled();
            self.settings.save();
        }
        if do_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // ------------------------------------------------------------------
    // UI: Image canvas
    // ------------------------------------------------------------------

    fn draw_image_canvas_ui(&mut self, ui: &mut egui::Ui) {
        // Fill the area with dark background; CentralPanel inside ui() uses show_inside
        egui::Frame::NONE.fill(BG_DARK).show(ui, |ui| {
            let screen_rect = ui.max_rect();
            
            // Allocate the whole viewport for drag interaction and clicks early
            // This captures background clicks properly regardless of image state.
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

            if let Some(texture) = self.texture_cache.get(self.current_index).cloned() {
                let img_size = texture.size_vec2();

                if canvas_resp.dragged() {
                    self.pan_offset += canvas_resp.drag_delta();
                }

                // Context menu for copying
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
                    });

                    // Compute display rect and draw image
                    let dest = self.compute_display_rect(img_size, screen_rect);
                    ui.painter().image(
                        texture.id(),
                        dest,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );

                    if self.settings.show_osd {
                        // HUD overlay — image counter + filename + zoom + dimensions + scale mode
                        let fname = self.image_files[self.current_index]
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        let zoom_pct = (self.zoom_factor * 100.0).round() as u32;
                        let img_w = img_size.x.round() as u32;
                        let img_h = img_size.y.round() as u32;
                        let mode_label = self.settings.scale_mode.label();
                        let hud = format!(
                            "{} / {}    {}    {}%    {}×{}    [{}]",
                            self.current_index + 1,
                            self.image_files.len(),
                            fname,
                            zoom_pct,
                            img_w,
                            img_h,
                            mode_label,
                        );
                        let hud_pos = screen_rect.left_bottom() + Vec2::new(12.0, -12.0);
                        ui.painter().text(
                            hud_pos,
                            Align2::LEFT_BOTTOM,
                            &hud,
                            FontId::proportional(13.0),
                            Color32::from_rgba_unmultiplied(220, 220, 240, 210),
                        );

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
}

impl eframe::App for ImageViewerApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.settings.resume_last_image && !self.image_files.is_empty() {
            self.settings.last_viewed_image = Some(self.image_files[self.current_index].clone());
            self.settings.save();
        }
    }

    /// Background logic: scanning, loading, auto-switch, keyboard, timers.
    /// Called before each ui() call (and also when hidden but repaint requested).
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
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
        self.process_loaded_images(ctx);
        self.check_auto_switch();
        self.handle_keyboard(ctx);

        // Apply deferred viewport commands
        if let Some(fs) = self.pending_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fs));
        }

        // Keep repainting while loading or auto-switching
        if self.settings.auto_switch || self.scanning || !self.loader.rx.is_empty() {
            ctx.request_repaint();
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

        // Settings panel overlay
        if self.show_settings {
            self.draw_settings_panel(&ctx);
        }

        // Goto dialog
        if self.show_goto {
            self.draw_goto_dialog(&ctx);
        }

        // EXIF window
        if self.show_exif_window {
            let mut close_exif = false;
            let mut close_and_copy = false;
            egui::Window::new("ℹ EXIF Information")
                .collapsible(false)
                .resizable(true)
                .default_width(400.0)
                .default_height(350.0)
                .show(&ctx, |ui| {
                    if let Some(text) = &self.cached_exif_text {
                        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            ui.label(text);
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        ui.horizontal(|ui| {
                            if styled_button(ui, "📋 Copy EXIF").clicked() {
                                close_and_copy = true;
                            }
                            if styled_button(ui, "Close").clicked() {
                                close_exif = true;
                            }
                        });
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

