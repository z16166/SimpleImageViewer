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

use crate::app::{CACHE_SIZE, HardwareTier, ImageViewerApp, compute_preload_budgets};
use crate::audio::AudioPlayer;
use crate::ipc::IpcMessage;
use crate::loader::{ImageLoader, TextureCache};
use crate::settings::{BrowseMode, Settings};
use crate::theme::SystemThemeCache;
use crate::ui::utils::{
    get_system_font_families, setup_fonts, setup_visuals, startup_font_family_list,
};
use eframe::egui::{self, Vec2};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

pub struct ImageViewerInit {
    pub settings: Settings,
    pub initial_image: Option<PathBuf>,
    pub ipc_rx: crossbeam_channel::Receiver<IpcMessage>,
    pub requested_target_format: eframe::egui_wgpu::RequestedSurfaceFormat,
    pub active_target_format: eframe::egui_wgpu::ActiveSurfaceFormat,
    pub requested_rgb10a2_pq_encode: eframe::egui_wgpu::RequestedRgb10a2PqEncode,
    pub gamma22_display_scale: eframe::egui_wgpu::Gamma22DisplayScale,
    pub vulkan_wsi_hdr_gates: eframe::egui_wgpu::VulkanWsiHdrGatesMailbox,
    #[cfg(target_os = "linux")]
    pub requested_vulkan_hdr_metadata: eframe::egui_wgpu::RequestedVulkanHdrMetadata,
    pub initial_hdr_monitor_selection: Option<crate::hdr::monitor::HdrMonitorSelection>,
}

impl ImageViewerApp {
    pub fn refresh_audio_devices(&mut self) {
        log::info!("[Audio] Refreshing audio device list...");
        self.cached_audio_devices = self.audio.list_devices();
    }

    pub fn new(cc: &eframe::CreationContext<'_>, init: ImageViewerInit) -> Self {
        let ImageViewerInit {
            settings,
            initial_image,
            ipc_rx,
            requested_target_format,
            active_target_format,
            requested_rgb10a2_pq_encode,
            gamma22_display_scale,
            vulkan_wsi_hdr_gates,
            #[cfg(target_os = "linux")]
            requested_vulkan_hdr_metadata,
            initial_hdr_monitor_selection,
        } = init;
        if settings.fullscreen {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        let mut theme_cache = SystemThemeCache::default();
        let cached_palette = settings.theme.resolve(&mut theme_cache);
        let directory_tree_theme =
            std::sync::Arc::new(parking_lot::Mutex::new(cached_palette.clone()));

        setup_visuals(&cc.egui_ctx, &settings, &cached_palette);
        if !setup_fonts(&cc.egui_ctx, &settings) {
            log::error!(
                "[Core] Persisted font '{}' failed validation. Reverting for safety.",
                settings.font_family
            );
            // We don't have self yet, but we can't easily change settings.
            // However, setup_fonts will have at least loaded CJK as fallback.
        }

        crate::ipc::register_ipc_wake_context(cc.egui_ctx.clone());
        let tray_cmd_rx =
            crate::app::tray_handlers::install_tray_event_handlers(cc.egui_ctx.clone());

        let (save_tx, save_rx) = crossbeam_channel::unbounded::<Settings>();
        let (save_error_tx, save_error_rx) = crossbeam_channel::unbounded::<String>();
        let saver_res = std::thread::Builder::new()
            .name("settings-saver".to_string())
            .spawn(move || {
                crate::app::background_yaml_saver::run_coalescing_periodic_saver(
                    save_rx,
                    crate::constants::BACKGROUND_YAML_SAVE_MIN_INTERVAL,
                    |settings| settings.save(),
                    |e| {
                        let _ = save_error_tx.send(e);
                    },
                );
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
                crate::app::background_yaml_saver::run_coalescing_periodic_saver(
                    hotkeys_save_rx,
                    crate::constants::BACKGROUND_YAML_SAVE_MIN_INTERVAL,
                    crate::hotkeys::io::save_hotkeys_file,
                    |e| {
                        let _ = hotkeys_save_error_tx.send(e);
                    },
                );
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
                crate::app::background_yaml_saver::run_coalescing_periodic_saver(
                    context_menu_save_rx,
                    crate::constants::BACKGROUND_YAML_SAVE_MIN_INTERVAL,
                    crate::context_menu::io::save_context_menu_file,
                    |e| {
                        let _ = context_menu_save_error_tx.send(e);
                    },
                );
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

        let hdr_callback_resources_prewarm =
            crate::hdr::renderer::HdrCallbackResourcesPrewarm::new_shared();
        let (wgpu_pipeline_cache, wgpu_adapter_info) = if let Some(state) =
            cc.wgpu_render_state.as_ref()
        {
            let adapter_info = state.adapter.get_info();
            let limits = state.device.limits();
            let gl_backend = adapter_info.backend == wgpu::Backend::Gl;
            let wg = crate::hdr::raw_demosaic_gpu::RAW_DEMOSAIC_WORKGROUP_SIZE;
            let raw_demosaic_compute_supported = limits.max_compute_invocations_per_workgroup
                >= 256
                && limits.max_compute_workgroup_size_x >= wg
                && limits.max_compute_workgroup_size_y >= wg;
            crate::loader::GPU_DEMOSAIC_SUPPORTED.store(
                !gl_backend && raw_demosaic_compute_supported,
                std::sync::atomic::Ordering::Relaxed,
            );
            if gl_backend || !raw_demosaic_compute_supported {
                log::debug!(
                    "[Loader] GPU RAW demosaic disabled at startup \
                     (backend={:?}, max_compute_invocations_per_workgroup={})",
                    adapter_info.backend,
                    limits.max_compute_invocations_per_workgroup
                );
            }
            let pipeline_cache =
                crate::wgpu_pipeline_cache::create_pipeline_cache(&state.device, &state.adapter);
            if let Some(format) = crate::hdr::renderer::predicted_hdr_callback_target_format(
                crate::hdr::surface::native_hdr_swapchain_requests_enabled(
                    settings.hdr_native_surface_enabled_effective(),
                    Some(adapter_info.backend),
                ),
                initial_hdr_monitor_selection
                    .as_ref()
                    .is_some_and(|selection| selection.hdr_supported),
                hdr_capabilities.candidate_texture_format,
                hdr_target_format,
            ) {
                hdr_callback_resources_prewarm.ensure_started(
                    &state.device,
                    format,
                    pipeline_cache.as_ref(),
                );
            }
            state.renderer.write().callback_resources.insert(
                crate::hdr::renderer::HdrCallbackResourcesPrewarmSlot(
                    hdr_callback_resources_prewarm.clone(),
                ),
            );
            (pipeline_cache.map(std::sync::Arc::new), Some(adapter_info))
        } else {
            crate::loader::GPU_DEMOSAIC_SUPPORTED
                .store(false, std::sync::atomic::Ordering::Relaxed);
            (None, None)
        };

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

        let (directory_tree_strip_preview_tx, directory_tree_strip_preview_rx) =
            crossbeam_channel::bounded(16);
        let (directory_tree_strip_inflight_release_tx, directory_tree_strip_inflight_release_rx) =
            crossbeam_channel::bounded(64);
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

        let (osd_event_tx, osd_event_rx) = crossbeam_channel::unbounded();
        let current_device_id = 1_u64;
        let loader_wgpu_device = cc.wgpu_render_state.as_ref().map(|s| s.device.clone());
        let loader = if let Some(state) = cc.wgpu_render_state.as_ref() {
            ImageLoader::new().with_wgpu(
                Some(state.device.clone()),
                Some(state.queue.clone()),
                current_device_id,
            )
        } else {
            ImageLoader::new()
        };
        // Defer neighbor/current preload until HDR output mode + decode headroom are known.
        // macOS EDR: uses NSScreen *potential* for decode (not dynamic *current*) — see
        // `src/hdr/monitor/macos.rs` and [`startup_preload_defer_can_release`].
        let preload_deferred_for_hdr_capacity =
            crate::hdr::surface::native_hdr_swapchain_requests_enabled(
                settings.hdr_native_surface_enabled_effective(),
                hdr_capabilities.backend,
            );
        let auto_hidden_directory_tree_nav =
            initial_image.is_some() && settings.show_directory_tree_nav;
        let mut app = Self {
            save_tx,
            initial_image,
            image_files: Vec::new(),
            cached_image_strip_path_index: None,
            file_byte_len_by_index: Vec::new(),
            file_modified_unix_by_index: Vec::new(),
            current_index: 0,
            scan_rx: None,
            scan_cancel: None,
            root_redraw_wake: None,
            directory_tree_theme,
            pending_directory_tree_repaint: false,
            pending_directory_tree_select_index: None,
            pending_directory_tree_state_sync: false,
            pending_directory_tree_sync_warning: None,
            directory_tree_sync_defer_frames: 0,
            scan_generation: 0,
            scan_results_pending_since: None,
            pending_preload_after_directory_scan: false,
            pending_preload_after_scan_last_attempt: None,
            directory_tree_strip_bootstrap_after_scan: false,
            directory_tree_strip_bootstrap_frames: 0,
            strip_preload_cooldown_frames: 0,
            strip_stale_retain_last_generation: u64::MAX,
            scanning: false,
            loader,
            texture_cache: TextureCache::new(CACHE_SIZE),
            hdr_capabilities,
            hdr_renderer,
            wgpu_pipeline_cache,
            wgpu_adapter_info,
            current_device_id,
            loader_wgpu_device,
            hdr_callback_resources_prewarm,
            hdr_target_format,
            hdr_monitor_state: crate::hdr::monitor::HdrMonitorState::with_initial_selection(
                initial_hdr_monitor_selection,
            ),
            cached_window_placement: None,
            cached_restore_placement: None,
            cached_directory_tree_window_placement: None,
            cached_directory_tree_restore_placement: None,
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
            #[cfg(target_os = "linux")]
            last_logged_linux_hdr_runtime_diag: None,
            #[cfg(feature = "preload-debug")]
            hdr_preload_gate_log: crate::app::preload_hdr_gate::GateLogState::default(),
            rgb10a2_pq_encode_requested: false,
            ultra_hdr_decode_capacity,
            ultra_hdr_decode_output_mode: initial_hdr_output_mode,
            preload_deferred_for_hdr_capacity,
            current_hdr_image: None,
            hdr_image_cache: std::collections::HashMap::new(),
            current_hdr_tiled_image: None,
            hdr_tiled_source_cache: std::collections::HashMap::new(),
            current_hdr_tiled_preview: None,
            hdr_tiled_preview_cache: std::collections::HashMap::new(),
            hdr_sdr_fallback_indices: std::collections::HashSet::new(),
            hdr_placeholder_fallback_indices: std::collections::HashSet::new(),
            hdr_raw_gpu_demosaic_pending_indices: std::collections::HashSet::new(),
            hdr_raw_gpu_demosaic_baked_indices: std::collections::HashSet::new(),
            hdr_raw_gpu_demosaic_pending_key_index: std::collections::HashMap::new(),
            raw_gpu_embedded_bootstrap_indices: std::collections::HashSet::new(),
            hdr_register_prewarm_repush_counts: std::collections::HashMap::new(),
            gpu_demosaic_failed_indices: std::collections::HashSet::new(),
            raw_gpu_demosaic_await_hdr_present: false,
            raw_demosaic_baked_notify: Arc::new(Mutex::new(Vec::new())),
            hdr_in_flight_fallback_refinements: std::collections::HashSet::new(),
            cpu_raw_refinement_pending_indices: std::collections::HashSet::new(),
            hq_tiled_preview_pending_indices: std::collections::HashSet::new(),
            deferred_sdr_uploads: std::collections::HashMap::new(),
            ultra_hdr_capacity_sensitive_indices: std::collections::HashSet::new(),
            animation: None,
            pan_offset: Vec2::ZERO,
            zoom_factor: 1.0,
            last_switch_time: Instant::now(),
            slideshow_paused: false,
            random_slideshow_order_ready: false,
            audio: AudioPlayer::new(),
            show_settings: settings.last_image_dir.is_none()
                && settings.transient_image_dir.is_none(),
            settings_tab: crate::app::SettingsTab::Library,
            about_icon_texture: None,
            images_ever_loaded: false,
            status_message: rust_i18n::t!("status.open_dir_hint").to_string(),
            error_message: None,
            is_font_error: false,
            modal_generation: 0,
            pending_fullscreen: None,
            pending_open_directory: false,
            folder_picker: crate::app::folder_picker::FolderPickerRuntime::new(),
            directory_tree: crate::app::DirectoryTreeRuntime::new(),
            auto_hidden_directory_tree_nav,
            directory_tree_strip_cache:
                crate::app::directory_tree_strip_cache::DirectoryTreeStripCache::default(),
            directory_tree_strip_compose_probe_cache:
                crate::app::directory_tree_strip_cache::DirectoryTreeStripComposeProbeCache::default(
                ),
            directory_tree_strip_tiled_attempted: std::collections::HashSet::new(),
            directory_tree_strip_cold_attempted: std::collections::HashSet::new(),
            directory_tree_strip_generate_inflight: std::collections::HashSet::new(),
            directory_tree_strip_preview_tx,
            directory_tree_strip_preview_rx,
            directory_tree_strip_inflight_release_tx,
            directory_tree_strip_inflight_release_rx,
            directory_tree_strip_pending_gpu_initial: VecDeque::new(),
            directory_tree_strip_pending_gpu_refined: VecDeque::new(),
            directory_tree_strip_pending_gpu_next_seq: 0,
            directory_tree_places_load_rx: None,
            font_families,
            font_families_rx: font_enumeration_rx,

            cached_music_count: None,
            cached_pixels_per_point: 1.0,
            active_modal: None,
            music_scan_rx: None,
            scanning_music: false,
            music_scan_cancel: None,
            music_scan_path: None,
            current_image_res: None,
            canvas_display_timing: crate::preload_debug::CanvasDisplayTiming::default(),
            raw_metadata: crate::app::view_status::RawMetadataStore::new(osd_event_tx.clone()),
            image_status: crate::app::view_status::ImageViewStatus::new(osd_event_tx.clone()),
            current_file_name: String::new(),
            cached_keyboard_hint: rust_i18n::t!("hint.keyboard").to_string(),
            cached_directory_tree_viewport_title: rust_i18n::t!("directory_tree.title").to_string(),
            directory_tree_viewport_title_sent: false,
            cached_frame_render_plan: None,
            cached_frame_hdr_render_path: None,
            frame_effective_hdr_monitor_selection: None,
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
            osd: crate::ui::osd::OsdRenderer::new(osd_event_rx),
            last_minimized: false,
            last_frame_time: Instant::now(),
            last_logic_shared_at: None,
            ipc_rx,
            animation_cache: std::collections::HashMap::new(),
            installed_display_modes: std::collections::HashMap::new(),
            tile_manager: None,
            tiled_primary_visible_scratch: HashSet::new(),
            tiled_visible_coords_scratch: Vec::new(),
            prefetched_tiles: std::collections::HashMap::new(),
            theme_cache,
            cached_palette,
            is_printing: Arc::new(AtomicBool::new(false)),
            print_status_rx: None,
            pending_anim_frames: HashMap::new(),
            last_mouse_wheel_nav: 0.0,
            last_canvas_rect: None,
            last_keyboard_nav: None,
            preload_budget_forward: budget_fwd,
            preload_budget_backward: budget_bwd,
            preload_memory: crate::app::preload_memory::PreloadMemorySnapshot::new(),
            cached_available_memory_mb: 0,
            cached_total_memory_mb: 0,
            prefetch_window_max_distance: crate::loader::DEFAULT_PREFETCH_WINDOW_DISTANCE,
            file_op_rx,
            file_op_tx,
            lightweight_file_op_tx,
            background_threads: crate::app::background_threads::BackgroundThreadJoiner::new(),
            context_menu_pos: None,
            context_menu_viewport: None,
            context_menu_label_cache: None,
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
            last_show_settings: settings.last_image_dir.is_none()
                && settings.transient_image_dir.is_none(),
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
            context_menu_exe_browse_requested: false,
            refresh_scan_in_progress: false,
            refresh_scan_slideshow_was_playing: false,
            refresh_anchor_path: None,
            refresh_strip_files_snapshot: None,
            pixel_data_source: None,
            pixel_hover_cache: None,
            pixel_region_first_point: None,
            tray_state: None,
            hidden_to_tray: false,
            pending_hide_to_tray: false,
            tray_cmd_rx,
            copy_cut_overwrite_if_exists: false,
            explicit_quit: false,
            settings,
        };
        for diagnostic in app.hdr_capabilities.startup_diagnostics() {
            log::info!("{diagnostic}");
        }
        #[cfg(target_os = "linux")]
        {
            crate::hdr::linux_diag::log_session_startup(
                app.settings.hdr_native_surface_enabled,
                app.settings.hdr_native_surface_enabled_effective(),
                app.hdr_capabilities.output_mode,
            );
            if !app.hdr_capabilities.native_presentation_enabled
                && app.hdr_capabilities.candidate_platform_path.is_some()
            {
                log::info!(
                    "[HDR] startup: native HDR swap-chain not active yet; watch for \
                     \"[HDR] app_active\" once WSI probing and runtime admission complete"
                );
            }
        }
        app.loader
            .set_hdr_target_capacity(app.ultra_hdr_decode_capacity);
        app.loader
            .set_hdr_tone_map_settings(app.effective_hdr_tone_map_settings());
        app.loader.set_output_mode(app.hdr_capabilities.output_mode);
        app.sync_loader_hdr_callback_upload_snapshot();
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
        if app.settings.show_directory_tree_nav {
            app.settings.browse_mode = BrowseMode::Tree;
            app.ensure_directory_tree_places_loaded();
            app.restore_saved_directory_tree_panel_layout();
            app.ensure_directory_tree_reveals_current_browse_dir();
        }

        if let Some(dir) = app.current_browse_directory() {
            app.reload_current_browse_directory(dir);
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

    /// Enqueues a best-effort background YAML write (trailing debounce, ~5s quiet period).
    /// `last_viewed_image` and other session state are persisted authoritatively in `on_exit`.
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
