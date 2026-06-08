use crate::app::{CACHE_SIZE, HardwareTier, ImageViewerApp, compute_preload_budgets};
use crate::audio::AudioPlayer;
use crate::ipc::IpcMessage;
use crate::loader::{ImageLoader, TextureCache};
use crate::settings::Settings;
use crate::theme::SystemThemeCache;
use crate::ui::utils::{
    get_system_font_families, setup_fonts, setup_visuals, startup_font_family_list,
};
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
        requested_target_format: eframe::egui_wgpu::RequestedSurfaceFormat,
        active_target_format: eframe::egui_wgpu::ActiveSurfaceFormat,
        requested_rgb10a2_pq_encode: eframe::egui_wgpu::RequestedRgb10a2PqEncode,
        gamma22_display_scale: eframe::egui_wgpu::Gamma22DisplayScale,
        vulkan_wsi_hdr_gates: eframe::egui_wgpu::VulkanWsiHdrGatesMailbox,
        #[cfg(target_os = "linux")]
        requested_vulkan_hdr_metadata: eframe::egui_wgpu::RequestedVulkanHdrMetadata,
        initial_hdr_monitor_selection: Option<crate::hdr::monitor::HdrMonitorSelection>,
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

        let mut hotkeys_load_error = None;
        let hotkeys_runtime = match crate::hotkeys::load_runtime_hotkeys_state() {
            Ok(state) => {
                for warning in &state.warnings {
                    log::warn!(
                        "[hotkeys] {}",
                        crate::app::localized_hotkey_warning(warning)
                    );
                }
                for conflict in &state.conflicts {
                    log::warn!(
                        "[hotkeys] conflict {}: {:?}",
                        conflict.key,
                        conflict.actions
                    );
                }
                state
            }
            Err(e) => {
                log::error!("[hotkeys] failed to load runtime hotkeys: {}", e);
                hotkeys_load_error = Some(e);
                crate::hotkeys::rebuild_runtime_state(
                    &crate::hotkeys::model::default_hotkey_config_file(),
                )
            }
        };
        let hotkeys_draft_config = hotkeys_runtime.config.clone();
        let (hotkeys_save_tx, hotkeys_save_rx) =
            crossbeam_channel::unbounded::<crate::hotkeys::model::HotkeyConfigFile>();
        let (hotkeys_save_error_tx, hotkeys_save_error_rx) =
            crossbeam_channel::unbounded::<String>();
        let hotkeys_saver_res = std::thread::Builder::new()
            .name("hotkeys-saver".to_string())
            .spawn(move || {
                while let Ok(mut cfg) = hotkeys_save_rx.recv() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    while let Ok(newer) = hotkeys_save_rx.try_recv() {
                        cfg = newer;
                    }
                    if let Err(e) = crate::hotkeys::io::save_hotkeys_file(&cfg) {
                        let _ = hotkeys_save_error_tx.send(e);
                    }
                }
            });
        let hotkeys_saver_handle = match hotkeys_saver_res {
            Ok(handle) => Some(handle),
            Err(e) => {
                log::error!("[Core] Failed to spawn hotkeys-saver thread: {}", e);
                None
            }
        };

        let context_menu_runtime = match crate::context_menu::load_runtime_context_menu_state() {
            Ok(state) => state,
            Err(e) => {
                log::error!("[context_menu] failed to load context menu config: {}", e);
                crate::context_menu::rebuild_runtime_state(
                    &crate::context_menu::model::default_context_menu_config_file(),
                )
            }
        };
        let context_menu_draft_config = context_menu_runtime.config.clone();
        let (context_menu_save_tx, context_menu_save_rx) =
            crossbeam_channel::unbounded::<crate::context_menu::model::ContextMenuConfigFile>();
        let (context_menu_save_error_tx, context_menu_save_error_rx) =
            crossbeam_channel::unbounded::<String>();
        let context_menu_saver_res = std::thread::Builder::new()
            .name("context-menu-saver".to_string())
            .spawn(move || {
                while let Ok(mut cfg) = context_menu_save_rx.recv() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    while let Ok(newer) = context_menu_save_rx.try_recv() {
                        cfg = newer;
                    }
                    if let Err(e) = crate::context_menu::io::save_context_menu_file(&cfg) {
                        let _ = context_menu_save_error_tx.send(e);
                    }
                }
            });
        let context_menu_saver_handle = match context_menu_saver_res {
            Ok(handle) => Some(handle),
            Err(e) => {
                log::error!("[Core] Failed to spawn context-menu-saver thread: {}", e);
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
        let hdr_target_format = cc.wgpu_render_state.as_ref().map(|s| s.target_format);
        let initial_render_output_mode = crate::hdr::monitor::effective_render_output_mode(
            hdr_target_format,
            initial_hdr_monitor_selection.as_ref(),
        );
        hdr_renderer.tone_map = settings.hdr_tone_map_settings_for_monitor(
            initial_hdr_monitor_selection.as_ref(),
            initial_render_output_mode,
        );
        let initial_hdr_output_mode = crate::hdr::monitor::effective_capability_output_mode(
            hdr_target_format,
            initial_hdr_monitor_selection.as_ref(),
        );
        let ultra_hdr_decode_capacity = crate::app::ultra_hdr_decode_capacity_for_output_mode(
            settings.hdr_tone_map_settings_for_monitor(
                initial_hdr_monitor_selection.as_ref(),
                initial_render_output_mode,
            ),
            initial_hdr_output_mode,
            initial_hdr_monitor_selection.as_ref(),
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
        let available_ram_mb = sys.available_memory() / (1024 * 1024);
        let (cpu_cache_mb, hdr_tile_cache_mb) =
            crate::app::memory_aware_tile_cache_budgets_mb(tier, available_ram_mb);
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .set_max_mb(cpu_cache_mb);
        crate::hdr::tiled::HDR_TILE_CACHE_MAX_BYTES.store(
            hdr_tile_cache_mb * 1024 * 1024,
            std::sync::atomic::Ordering::Relaxed,
        );
        log::info!(
            "Tile cache budgets: SDR={} MB, HDR={} MB (available RAM={} MB)",
            cpu_cache_mb,
            hdr_tile_cache_mb,
            available_ram_mb
        );

        let (file_op_tx, file_op_rx) = crossbeam_channel::unbounded();

        let file_op_tx_for_menu_worker = file_op_tx.clone();
        let (lightweight_file_op_tx, lightweight_file_op_rx) =
            crossbeam_channel::unbounded::<crate::app::LightweightFileOpJob>();
        std::thread::Builder::new()
            .name("siv-context-file-ops".into())
            .spawn(move || {
                while let Ok(job) = lightweight_file_op_rx.recv() {
                    match job {
                        crate::app::LightweightFileOpJob::Exif(path) => {
                            let data = crate::app::extract_exif(&path);
                            let _ = file_op_tx_for_menu_worker
                                .send(crate::app::FileOpResult::Exif(path, data));
                        }
                        crate::app::LightweightFileOpJob::Xmp(path) => {
                            let data = crate::app::extract_xmp(&path);
                            let _ = file_op_tx_for_menu_worker
                                .send(crate::app::FileOpResult::Xmp(path, data));
                        }
                        crate::app::LightweightFileOpJob::Wallpaper => {
                            let current_wallpaper = wallpaper::get().ok();
                            let (monitors, supports_per_monitor) =
                                crate::ui::dialogs::wallpaper::probe_windows_wallpaper_targets();
                            let _ = file_op_tx_for_menu_worker.send(
                                crate::app::FileOpResult::Wallpaper {
                                    current: current_wallpaper,
                                    monitors,
                                    supports_per_monitor,
                                },
                            );
                        }
                    }
                }
            })
            .expect("spawn siv-context-file-ops worker");

        let (font_families_tx, font_families_rx) = crossbeam_channel::bounded::<Vec<String>>(1);
        let font_enumeration_rx = match std::thread::Builder::new()
            .name("font-families".to_string())
            .spawn(move || {
                let t0 = Instant::now();
                let families = get_system_font_families();
                log::info!(
                    "[startup] system font enumeration (background): {} ms, {} families",
                    t0.elapsed().as_millis(),
                    families.len()
                );
                let _ = font_families_tx.send(families);
            }) {
            Ok(_) => Some(font_families_rx),
            Err(e) => {
                log::error!(
                    "[Core] Failed to spawn font-families thread, falling back to sync enumeration: {}",
                    e
                );
                None
            }
        };
        let font_families = if font_enumeration_rx.is_some() {
            startup_font_family_list(&settings)
        } else {
            get_system_font_families()
        };

        let mut app = Self {
            save_tx,
            initial_image,
            image_files: Vec::new(),
            file_byte_len_by_index: Vec::new(),
            current_index: 0,
            scan_rx: None,
            scan_cancel: None,
            scanning: false,
            loader: ImageLoader::new(),
            texture_cache: TextureCache::new(CACHE_SIZE),
            hdr_capabilities,
            hdr_renderer,
            hdr_target_format,
            hdr_monitor_state: crate::hdr::monitor::HdrMonitorState::with_initial_selection(
                initial_hdr_monitor_selection,
            ),
            cached_window_placement: None,
            cached_restore_placement: None,
            requested_target_format,
            active_target_format,
            requested_rgb10a2_pq_encode,
            gamma22_display_scale,
            vulkan_wsi_hdr_gates,
            #[cfg(target_os = "linux")]
            requested_vulkan_hdr_metadata,
            #[cfg(target_os = "linux")]
            last_vulkan_hdr_metadata: None,
            last_logged_swap_chain_format_request: None,
            rgb10a2_pq_encode_requested: false,
            ultra_hdr_decode_capacity,
            ultra_hdr_decode_output_mode: initial_hdr_output_mode,
            current_hdr_image: None,
            hdr_image_cache: std::collections::HashMap::new(),
            current_hdr_tiled_image: None,
            hdr_tiled_source_cache: std::collections::HashMap::new(),
            current_hdr_tiled_preview: None,
            hdr_tiled_preview_cache: std::collections::HashMap::new(),
            hdr_sdr_fallback_indices: std::collections::HashSet::new(),
            hdr_placeholder_fallback_indices: std::collections::HashSet::new(),
            hdr_in_flight_fallback_refinements: std::collections::HashSet::new(),
            deferred_sdr_uploads: std::collections::HashMap::new(),
            ultra_hdr_capacity_sensitive_indices: std::collections::HashSet::new(),
            animation: None,
            pan_offset: Vec2::ZERO,
            zoom_factor: 1.0,
            last_switch_time: Instant::now(),
            slideshow_paused: false,
            random_slideshow_order_ready: false,
            audio: AudioPlayer::new(),
            show_settings: settings.last_image_dir.is_none(),
            settings_tab: crate::app::SettingsTab::Library,
            about_icon_texture: None,
            images_ever_loaded: false,
            status_message: rust_i18n::t!("status.open_dir_hint").to_string(),
            error_message: None,
            is_font_error: false,
            modal_generation: 0,
            pending_fullscreen: None,
            font_families,
            font_families_rx: font_enumeration_rx,
            temp_font_size: None,
            generation: 0,
            prefetch_prev_generation: None,
            cached_music_count: None,
            cached_pixels_per_point: 1.0,
            active_modal: None,
            music_scan_rx: None,
            scanning_music: false,
            music_scan_cancel: None,
            music_scan_path: None,
            current_image_res: None,
            raw_osd_by_index: std::collections::HashMap::new(),
            current_osd_file_name: String::new(),
            cached_keyboard_hint: rust_i18n::t!("hint.keyboard").to_string(),
            prev_texture: None,
            prev_hdr_image: None,
            prev_transition_rect: None,
            transition_start: None,
            transition_settled_at: None,
            transition_end_hold: false,
            pending_transition_target: None,
            last_background_upload_at: None,
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
            preload_memory_sys: sysinfo::System::new_with_specifics(
                sysinfo::RefreshKind::nothing()
                    .with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()),
            ),
            file_op_rx,
            file_op_tx,
            lightweight_file_op_tx,
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
            last_show_settings: settings.last_image_dir.is_none(),
            music_hud_drag_offset: Vec2::ZERO,
            hotkeys_runtime,
            hotkeys_draft_config,
            hotkeys_save_error_rx,
            hotkeys_save_tx,
            hotkeys_saver_handle,
            last_hotkeys_save_error: None,
            hotkeys_apply_success_at: None,
            hotkeys_load_error,
            startup_hotkeys_alert_shown: false,
            hotkeys_capture_target: None,
            hotkeys_selected_row: None,
            hotkeys_add_row_dialog_open: false,
            hotkeys_add_row_action: crate::hotkeys::model::HotkeyActionId::NextImage,
            hotkeys_add_row_capture_active: false,
            hotkeys_add_row_captured_key: None,
            hotkeys_add_row_need_key_hint: false,
            context_menu_runtime,
            context_menu_draft_config,
            context_menu_save_error_rx,
            context_menu_save_tx,
            context_menu_saver_handle,
            last_context_menu_save_error: None,
            context_menu_apply_success_at: None,
            context_menu_apply_error: None,
            context_menu_selected_row: None,
            context_menu_scroll_to_selected: false,
            context_menu_drag_row: None,
            context_menu_help_open: false,
            context_menu_edit_dialog_open: false,
            context_menu_edit_target: None,
            context_menu_edit_draft: crate::context_menu::model::EditableContextMenuEntry::default(
            ),
            refresh_scan_in_progress: false,
            refresh_scan_slideshow_was_playing: false,
            refresh_anchor_path: None,
            settings,
        };
        for diagnostic in app.hdr_capabilities.startup_diagnostics() {
            log::info!("{diagnostic}");
        }
        #[cfg(target_os = "linux")]
        {
            log::info!(
                "[HDR] linux presentation: wayland_session={} hdr_platform_eligible={} output_mode={:?}",
                crate::hdr::platform::is_wayland_session(),
                crate::hdr::platform::linux_native_hdr_platform_eligible(),
                app.hdr_capabilities.output_mode,
            );
        }
        app.loader
            .set_hdr_target_capacity(app.ultra_hdr_decode_capacity);
        app.loader
            .set_hdr_tone_map_settings(app.effective_hdr_tone_map_settings());
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

    pub(crate) fn queue_hotkeys_save(&self) {
        let _ = self
            .hotkeys_save_tx
            .send(self.hotkeys_runtime.config.clone());
    }

    pub(crate) fn queue_context_menu_save(&self) {
        let _ = self
            .context_menu_save_tx
            .send(self.context_menu_runtime.config.clone());
    }
}
