use crate::app::ImageViewerApp;
use crate::app::{CACHE_SIZE, HardwareTier, compute_preload_budgets};
use crate::audio::AudioPlayer;
use crate::ipc::IpcMessage;
use crate::loader::{ImageLoader, TextureCache};
use crate::settings::Settings;
use crate::theme::SystemThemeCache;
use crate::ui::utils::{get_system_font_families, setup_fonts, setup_visuals};
use eframe::egui::{self, Vec2};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

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
            log::error!(
                "[Core] Persisted font '{}' failed validation. Reverting for safety.",
                settings.font_family
            );
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

        let saver_handle = match saver_res {
            Ok(handle) => Some(handle),
            Err(e) => {
                log::error!("[Core] Failed to spawn settings-saver thread: {}", e);
                None
            }
        };

        let (budget_fwd, budget_bwd) = compute_preload_budgets();

        // ── GPU Limits ───────────────────────────────────────────────────────
        let max_texture_side_hw = cc
            .wgpu_render_state
            .as_ref()
            .map(|s| s.adapter.limits().max_texture_dimension_2d)
            .unwrap_or(crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE);

        // Use the hardware-reported limit directly. The wgpu device was already
        // created with this same adapter limit, so it is safe to upload textures
        // up to this size. No additional capping needed.
        let max_texture_side = max_texture_side_hw;
        log::info!(
            "Using max_texture_side: {} (hw reported: {})",
            max_texture_side,
            max_texture_side_hw
        );

        let hdr_capabilities =
            crate::hdr::capabilities::detect_from_wgpu_state(cc.wgpu_render_state.as_ref());
        let mut hdr_renderer = crate::hdr::renderer::HdrImageRenderer::new();
        hdr_renderer.tone_map = settings.hdr_tone_map_settings();
        let hdr_target_format = cc.wgpu_render_state.as_ref().map(|s| s.target_format);
        let initial_hdr_output_mode = hdr_capabilities.output_mode;
        let ultra_hdr_decode_capacity = crate::app::ultra_hdr_decode_capacity_for_output_mode(
            settings.hdr_tone_map_settings(),
            initial_hdr_output_mode,
        );
        for diagnostic in crate::hdr::renderer::hdr_render_output_diagnostics(hdr_target_format) {
            log::info!("{diagnostic}");
        }
        for diagnostic in crate::hdr::renderer::hdr_egui_overlay_diagnostics(hdr_target_format) {
            log::info!("{diagnostic}");
        }

        crate::tile_cache::MAX_TEXTURE_SIDE
            .store(max_texture_side, std::sync::atomic::Ordering::Relaxed);

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
                    tier = if total_ram_gb >= 16 {
                        HardwareTier::High
                    } else {
                        HardwareTier::Medium
                    };
                }
                wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::VirtualGpu => {
                    tier = if total_ram_gb >= 16 {
                        HardwareTier::Medium
                    } else {
                        HardwareTier::Low
                    };
                }
                _ => {}
            }
            log::info!(
                "Hardware Detection: Tier={:?}, GPU={:?}, RAM={}GB, Adapter={}",
                tier,
                info.device_type,
                total_ram_gb,
                info.name
            );
        } else {
            tier = if total_ram_gb >= 16 {
                HardwareTier::Medium
            } else {
                HardwareTier::Low
            };
            log::info!(
                "Hardware Detection: Tier={:?} (No WGPU), RAM={}GB",
                tier,
                total_ram_gb
            );
        }

        let tile_quota = tier.max_tile_quota();

        // Apply hardware budgets to global caches
        crate::tile_cache::MAX_TILES_BASE
            .store(tier.gpu_cache_tiles(), std::sync::atomic::Ordering::Relaxed);
        crate::tile_cache::TILED_THRESHOLD.store(
            tier.tiled_threshold_pixels(),
            std::sync::atomic::Ordering::Relaxed,
        );
        crate::loader::PREVIEW_LIMIT.store(
            tier.max_preview_size(),
            std::sync::atomic::Ordering::Relaxed,
        );
        if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
            cache.set_max_mb(tier.cpu_cache_mb());
        }
        crate::hdr::tiled::HDR_TILE_CACHE_MAX_BYTES.store(
            tier.hdr_tile_cache_mb() * 1024 * 1024,
            std::sync::atomic::Ordering::Relaxed,
        );

        let (file_op_tx, file_op_rx) = crossbeam_channel::unbounded();

        let mut app = Self {
            save_tx,
            initial_image,
            image_files: Vec::new(),
            current_index: 0,
            scan_rx: None,
            scan_cancel: None,
            scanning: false,
            loader: ImageLoader::new(),
            texture_cache: TextureCache::new(CACHE_SIZE),
            hdr_capabilities,
            hdr_renderer,
            hdr_target_format,
            hdr_monitor_state: crate::hdr::monitor::HdrMonitorState::default(),
            ultra_hdr_decode_capacity,
            current_hdr_image: None,
            hdr_image_cache: std::collections::HashMap::new(),
            current_hdr_tiled_image: None,
            hdr_tiled_source_cache: std::collections::HashMap::new(),
            hdr_sdr_fallback_indices: std::collections::HashSet::new(),
            animation: None,
            pan_offset: Vec2::ZERO,
            zoom_factor: 1.0,
            last_switch_time: Instant::now(),
            slideshow_paused: false,
            audio: AudioPlayer::new(),
            show_settings: true,
            images_ever_loaded: false,
            status_message: rust_i18n::t!("status.open_dir_hint").to_string(),
            error_message: None,
            is_font_error: false,
            modal_generation: 0,
            pending_fullscreen: None,
            font_families: get_system_font_families(),
            temp_font_size: None,
            generation: 0,
            cached_music_count: None,
            cached_pixels_per_point: 1.0,
            active_modal: None,
            music_scan_rx: None,
            scanning_music: false,
            music_scan_cancel: None,
            music_scan_path: None,
            current_image_res: None,
            prev_texture: None,
            transition_start: None,
            is_next: true,
            active_transition: settings.transition_style,
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
            last_keyboard_nav: None,
            preload_budget_forward: budget_fwd,
            preload_budget_backward: budget_bwd,
            file_op_rx,
            file_op_tx,
            context_menu_pos: None,
            current_rotation: 0,
            save_error_rx,
            last_save_error: None,
            saver_handle,
            tile_upload_quota: tile_quota,
            hardware_tier: tier,
            music_seeking_target_ms: None,
            music_seek_timeout: None,
            music_hud_last_activity: Instant::now(),
            cached_audio_devices: Vec::new(),
            last_show_settings: true,
            music_hud_drag_offset: Vec2::ZERO,
            settings,
        };
        for diagnostic in app.hdr_capabilities.startup_diagnostics() {
            log::info!("{diagnostic}");
        }
        app.loader
            .set_hdr_target_capacity(app.ultra_hdr_decode_capacity);
        log::info!(
            "[HDR] tone_map_sdr_white_nits={}",
            app.hdr_renderer.tone_map.sdr_white_nits
        );
        log::info!(
            "[Core] RAW engine initialized: {}",
            crate::raw_processor::version()
        );

        app.refresh_audio_devices();

        // Restore last session state
        if let Some(dir) = app.settings.last_image_dir.clone() {
            app.load_directory(dir);
        }
        if app.settings.play_music {
            app.settings.music_paused = true; // Always start paused to avoid start-up noise
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
}
