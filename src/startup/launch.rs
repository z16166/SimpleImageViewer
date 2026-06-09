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

use eframe::egui;
use std::time::Instant;

use super::icon::load_icon;
use super::logging::{init_logging, log_env_info, shutdown_logger};
use super::panic::setup_panic_hook;
use super::phases::{
    startup_log_captured_phases,
    startup_log_phase,
};

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
use super::wgpu::{
    apply_dx12_preprobe_to_wgpu_setup, register_dx12_cache_validate_join_for_exit,
    spawn_dx12_cache_validate_thread, spawn_dx12_preprobe_thread,
    take_and_join_dx12_cache_validate_thread, Dx12PreprobeOutcome,
};
#[cfg(all(
    target_os = "windows",
    target_arch = "aarch64",
    not(feature = "legacy_win7")
))]
use super::wgpu::apply_windows_arm64_default_wgpu_backends;

pub fn run() -> eframe::Result {
    crate::allocator_tuning::configure_mimalloc_for_image_viewer();

    #[cfg(target_os = "windows")]
    {
        #[cfg(feature = "legacy_win7")]
        unsafe {
            if std::env::var("WGPU_BACKEND").is_err() {
                // Force choice of ANGLE (OpenGL ES over DX11) for Windows 7 compatibility
                std::env::set_var("WGPU_BACKEND", "gl");
                std::env::set_var("WGPU_GL_BACKEND", "angle");
            }
        }
        #[cfg(not(feature = "legacy_win7"))]
        {
            // Environment variable hacks are removed in favor of explicit adapter probing below
        }
    }

    // 1. Parse initial image from arguments (needed for IPC)
    let mut initial_image = None;
    if let Some(arg) = std::env::args_os().nth(1) {
        let pic_path = std::path::PathBuf::from(arg);
        if pic_path.is_file() {
            initial_image = Some(pic_path);
        }
    }

    // 2. IPC Single-instance check
    let (ipc_tx, ipc_rx) = crossbeam_channel::unbounded();
    let no_recursive = initial_image.is_some();
    if crate::ipc::setup_or_forward_args(ipc_tx, initial_image.as_ref(), no_recursive) {
        // We Successfully forwarded to another instance, exit.
        shutdown_logger();
        std::process::exit(0);
    }

    // 3. Primary Instance Initialization
    let startup_t0 = Instant::now();
    let mut prev = startup_t0;
    #[cfg(feature = "startup-timing")]
    let mut startup_prelog_phases = Vec::new();

    // Install the Windows SEH exception filter as early as possible.
    // This catches native crashes (ACCESS_VIOLATION, STACK_OVERFLOW, etc.)
    // that bypass Rust's panic mechanism and would otherwise cause a
    // silent exit with no diagnostic output.
    #[cfg(target_os = "windows")]
    crate::seh_handler::install();
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(
        &mut startup_prelog_phases,
        &mut prev,
        startup_t0,
        "seh_handler::install",
    );

    let mut settings = crate::settings::Settings::load();
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(
        &mut startup_prelog_phases,
        &mut prev,
        startup_t0,
        "Settings::load",
    );

    let init_logging_phases = init_logging();
    #[cfg(feature = "startup-timing")]
    let init_logging_phase =
        startup_phase_at(&mut prev, startup_t0, "init_logging", Instant::now());
    #[cfg(feature = "startup-timing")]
    startup_log_captured_phases(&startup_prelog_phases);
    startup_log_captured_phases(&init_logging_phases);
    #[cfg(feature = "startup-timing")]
    startup_log_captured_phase(&init_logging_phase);
    #[cfg(feature = "startup-timing")]
    startup_reset_after_diagnostics(&mut prev);
    #[cfg(not(feature = "startup-timing"))]
    startup_log_phase(&mut prev, startup_t0, "init_logging");

    let mimalloc_startup_label = match crate::allocator_tuning::mimalloc_version() {
        315 => "mimalloc version 315 + image policy",
        _ => "mimalloc version unexpected + image policy",
    };
    startup_log_phase(&mut prev, startup_t0, mimalloc_startup_label);

    let env_info = log_env_info();
    startup_log_phase(&mut prev, startup_t0, "log_env_info");

    crate::hdr::tiled::configure_hdr_tile_cache_budget_from_system_memory();
    startup_log_phase(&mut prev, startup_t0, "hdr_tile_cache_budget");

    #[cfg(target_os = "windows")]
    {
        crate::wic::init_rayon_with_com();
        crate::wic::spawn_wic_discovery();
    }
    startup_log_phase(
        &mut prev,
        startup_t0,
        "wic_init_rayon + spawn_wic_discovery",
    );

    // Initialize locale — detect from OS if not yet configured
    if settings.language.is_empty() {
        settings.language = crate::settings::detect_system_language();
    }
    rust_i18n::set_locale(&settings.language);

    // NOW setup the panic hook - with logging AND correct language ready
    setup_panic_hook();
    startup_log_phase(&mut prev, startup_t0, "locale + set_locale + panic_hook");

    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    let cached_preprobe: Option<crate::wgpu_preprobe_cache::WgpuPreprobeCache> =
        match crate::wgpu_preprobe_cache::load() {
            Some(cache) if cache.format_version == crate::wgpu_preprobe_cache::FORMAT_VERSION => {
                Some(cache)
            }
            Some(cache) => {
                log::warn!(
                    "[startup] wgpu preprobe cache has unsupported format_version {} in {}; ignoring",
                    cache.format_version,
                    crate::wgpu_preprobe_cache::cache_path().display(),
                );
                None
            }
            None => None,
        };

    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    let (dx12_cache_validate_join, dx12_preprobe_rx): (
        Option<std::thread::JoinHandle<()>>,
        Option<std::sync::mpsc::Receiver<Option<Dx12PreprobeOutcome>>>,
    ) = if let Some(ref cache) = cached_preprobe {
        (spawn_dx12_cache_validate_thread(cache.force_dx12), None)
    } else {
        (None, Some(spawn_dx12_preprobe_thread()))
    };

    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    if let Some(h) = dx12_cache_validate_join {
        register_dx12_cache_validate_join_for_exit(h);
    }
    // Apply command-line overrides to settings
    if let Some(ref path) = initial_image {
        if let Some(parent) = path.parent() {
            settings.last_image_dir = Some(parent.to_path_buf());
        }
        settings.auto_switch = false;
        settings.recursive = false;
    }

    let fullscreen = settings.fullscreen;

    let saved_inner_size = settings.startup_inner_size();
    let saved_outer_position = settings.startup_outer_position();

    let app_icon = load_icon();
    startup_log_phase(&mut prev, startup_t0, "load_icon");

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(rust_i18n::t!("app.title").to_string())
        .with_inner_size(saved_inner_size)
        .with_min_inner_size([400.0, 300.0])
        .with_decorations(true)
        .with_fullscreen(fullscreen)
        // Maximize is applied from the app after the hidden window is created.
        .with_maximized(false)
        .with_icon(app_icon);
    if let Some(pos) = saved_outer_position {
        viewport = viewport.with_position(pos);
    }
    startup_log_phase(&mut prev, startup_t0, "viewport_builder + overrides");

    let mut wgpu_setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    #[cfg(all(
        target_os = "windows",
        target_arch = "aarch64",
        not(feature = "legacy_win7")
    ))]
    apply_windows_arm64_default_wgpu_backends(&mut wgpu_setup);
    wgpu_setup.device_descriptor = std::sync::Arc::new(|adapter| {
        let info = adapter.get_info();
        crate::startup_info!("Graphics Adapter Info: {} ({:?})", info.name, info.backend);
        if info.backend == eframe::wgpu::Backend::Gl {
            log::warn!("Running in compatibility mode (OpenGL/Compatibility).");
        }

        let base_limits = if info.backend == eframe::wgpu::Backend::Gl {
            eframe::wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            eframe::wgpu::Limits::default()
        };

        // Request the GPU's actual texture limit rather than a hardcoded 8192.
        // This allows panoramic images (e.g. 16380×1538) to be loaded as a single
        // texture on hardware that supports it, avoiding the slow tiled preview path.
        // On limited GPUs (e.g. VMware Mesa3D), the adapter will report 8192 and
        // the device will be created safely with that lower limit.
        let hw_max_texture = adapter.limits().max_texture_dimension_2d;
        let adapter_limits = adapter.limits();
        crate::startup_info!("GPU max_texture_dimension_2d: {}", hw_max_texture);
        crate::startup_info!(
            "GPU max_storage_buffer_binding_size: {}",
            adapter_limits.max_storage_buffer_binding_size
        );
        crate::startup_info!(
            "GPU max_storage_buffers_per_shader_stage: {}",
            adapter_limits.max_storage_buffers_per_shader_stage
        );
        crate::startup_info!(
            "GPU max_storage_textures_per_shader_stage: {}",
            adapter_limits.max_storage_textures_per_shader_stage
        );
        crate::startup_info!(
            "GPU max_compute_invocations_per_workgroup: {}",
            adapter_limits.max_compute_invocations_per_workgroup
        );

        eframe::wgpu::DeviceDescriptor {
            label: Some("egui wgpu device"),
            required_limits: eframe::wgpu::Limits {
                max_texture_dimension_2d: hw_max_texture,
                max_storage_buffer_binding_size: adapter_limits.max_storage_buffer_binding_size,
                max_buffer_size: adapter_limits.max_buffer_size,
                max_storage_buffers_per_shader_stage: adapter_limits
                    .max_storage_buffers_per_shader_stage,
                max_storage_textures_per_shader_stage: adapter_limits
                    .max_storage_textures_per_shader_stage,
                max_compute_workgroup_storage_size: adapter_limits
                    .max_compute_workgroup_storage_size,
                max_compute_invocations_per_workgroup: adapter_limits
                    .max_compute_invocations_per_workgroup,
                max_compute_workgroup_size_x: adapter_limits.max_compute_workgroup_size_x,
                max_compute_workgroup_size_y: adapter_limits.max_compute_workgroup_size_y,
                max_compute_workgroup_size_z: adapter_limits.max_compute_workgroup_size_z,
                max_compute_workgroups_per_dimension: adapter_limits
                    .max_compute_workgroups_per_dimension,
                ..base_limits
            },
            ..Default::default()
        }
    });

    startup_log_phase(&mut prev, startup_t0, "wgpu_setup_body (device_descriptor)");

    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    if let Some(ref cache) = cached_preprobe {
        apply_dx12_preprobe_to_wgpu_setup(&mut wgpu_setup, cache.force_dx12, true);
        crate::startup_info!(
            "[startup] wgpu preprobe: applied cache {} (main thread will not wait; background thread validates)",
            crate::wgpu_preprobe_cache::cache_path().display()
        );
        startup_log_phase(
            &mut prev,
            startup_t0,
            "wgpu dx12 preprobe (yaml cache; no recv wait)",
        );
    }

    #[cfg(feature = "startup-timing")]
    let hdr_spawn_start = Instant::now();
    let (preferred_hdr_target_format, hdr_environment_probe) =
        crate::hdr::surface::preferred_native_hdr_target_format_for_environment(
            settings.hdr_native_surface_enabled_effective(),
            settings.window_spawn_top_left_for_hdr(),
        );
    crate::startup_info!(
        "[startup] hdr spawn-monitor / native surface preset: {} ms",
        hdr_spawn_start.elapsed().as_millis()
    );
    startup_log_phase(
        &mut prev,
        startup_t0,
        "hdr_preferred_format + environment_probe",
    );

    let initial_hdr_monitor_selection =
        crate::hdr::surface::initial_monitor_selection_from_environment_probe(
            &hdr_environment_probe,
        );
    #[cfg(feature = "startup-timing")]
    {
        for diagnostic in crate::hdr::surface::native_hdr_surface_request_diagnostics(
            settings.hdr_native_surface_enabled_effective(),
            preferred_hdr_target_format,
        ) {
            crate::startup_info!("{diagnostic}");
        }
    }
    crate::startup_info!("[HDR] environment_probe={hdr_environment_probe:?}");

    #[cfg(target_os = "linux")]
    {
        crate::startup_info!(
            "[HDR] linux session: wayland={} platform_eligible={} native_surface_request_effective={}",
            crate::hdr::platform::is_wayland_session(),
            crate::hdr::platform::linux_native_hdr_platform_eligible(),
            settings.hdr_native_surface_enabled_effective(),
        );
    }

    // Shared mailbox the app writes into when the active monitor's HDR
    // capability changes (drag between HDR and SDR monitor); the patched
    // egui-wgpu Painter polls it every frame and hot-swaps the swap-chain
    // target format.
    let requested_target_format = eframe::egui_wgpu::RequestedSurfaceFormat::new();
    let requested_rgb10a2_pq_encode = eframe::egui_wgpu::RequestedRgb10a2PqEncode::new();
    let gamma22_display_scale = eframe::egui_wgpu::Gamma22DisplayScale::new();
    let vulkan_wsi_hdr_gates = eframe::egui_wgpu::VulkanWsiHdrGatesMailbox::new();
    let requested_vulkan_hdr_metadata = eframe::egui_wgpu::RequestedVulkanHdrMetadata::new();

    // Reverse-direction mailbox: the painter publishes the live active
    // swap-chain format here after every successful runtime hot-swap. We
    // need this because `egui_wgpu::RenderState` derives `Clone` and eframe
    // stores a CLONE in `Frame`, so mutating the painter's
    // `render_state.target_format` is invisible to the app via
    // `frame.wgpu_render_state()`. Without this mailbox the OSD freezes on
    // the very first runtime swap.
    let active_target_format = eframe::egui_wgpu::ActiveSurfaceFormat::new();

    // Centering must NOT compete with the saved outer-position recall: when
    // the user previously closed the window on (e.g.) the HDR monitor we want
    // to reopen there, not snap back to the primary monitor's centre. eframe
    // applies `centered=true` AFTER `with_position(...)` in winit setup, so
    // leaving it on silently overrides our recall.
    let center_window_on_open = saved_outer_position.is_none();

    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    if let Some(dx12_preprobe_rx) = dx12_preprobe_rx {
        #[cfg(feature = "startup-timing")]
        let recv_wait = Instant::now();
        let maybe_outcome = dx12_preprobe_rx
            .recv()
            .expect("wgpu dx12 preprobe thread exited without sending a result");
        #[cfg(feature = "startup-timing")]
        let main_wait_ms = recv_wait.elapsed().as_millis();

        if let Some(outcome) = maybe_outcome {
            let probe_force = outcome.has_real_dx12;
            apply_dx12_preprobe_to_wgpu_setup(&mut wgpu_setup, probe_force, false);
            if let Err(e) = crate::wgpu_preprobe_cache::save(probe_force) {
                log::warn!(
                    "[startup] failed to save wgpu preprobe cache {}: {}",
                    crate::wgpu_preprobe_cache::cache_path().display(),
                    e
                );
            } else {
                crate::startup_info!(
                    "[startup] wgpu preprobe: wrote cache {}",
                    crate::wgpu_preprobe_cache::cache_path().display()
                );
            }
            crate::startup_info!(
                "[startup] wgpu pre-probe enumerate_adapters: {} ms (adapter count {}); main recv wait: {} ms",
                outcome.enumerate_ms,
                outcome.adapter_count,
                main_wait_ms
            );
        } else {
            log::error!(
                "[startup] wgpu dx12 preprobe failed; using default wgpu backends, cache file unchanged ({})",
                crate::wgpu_preprobe_cache::cache_path().display()
            );
        }
        startup_log_phase(&mut prev, startup_t0, "wgpu dx12 preprobe recv + apply");
    }

    // Fullscreen uses borderless native fullscreen, not WS_SHOWMAXIMIZED; the patched
    // eframe first-frame show path only applies to maximized windowed restore.
    let first_frame_show_maximized = settings.window_maximized && !fullscreen;

    let native_options = eframe::NativeOptions {
        viewport,
        centered: center_window_on_open,
        first_frame_show_maximized,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(wgpu_setup),
            preferred_target_format: preferred_hdr_target_format,
            requested_target_format: requested_target_format.clone(),
            active_target_format: active_target_format.clone(),
            requested_rgb10a2_pq_encode: requested_rgb10a2_pq_encode.clone(),
            gamma22_display_scale: gamma22_display_scale.clone(),
            vulkan_wsi_hdr_gates: vulkan_wsi_hdr_gates.clone(),
            requested_vulkan_hdr_metadata: requested_vulkan_hdr_metadata.clone(),
            ..Default::default()
        },
        // Dithering assumes SDR gamma-space output. Leave it off when we ask
        // for a float HDR target; egui-wgpu falls back safely if unsupported.
        dithering: preferred_hdr_target_format.is_none(),
        ..Default::default()
    };
    startup_log_phase(
        &mut prev,
        startup_t0,
        "hdr diagnostics + NativeOptions (before run_native)",
    );

    #[cfg(target_os = "windows")]
    crate::seh_handler::reinstall_top_level_filter();

    log::info!(
        "[startup] Main-thread prep before window/event loop: {} ms total",
        prev.duration_since(startup_t0).as_millis()
    );

    let result = eframe::run_native(
        "Simple Image Viewer",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(crate::app::ImageViewerApp::new(
                cc,
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
                initial_hdr_monitor_selection.clone(),
            )) as Box<dyn eframe::App>)
        }),
    );

    // If `run_native` returns `Err`, `on_exit` may not run; still join so yaml can finish.
    // Normal Windows close uses `process::exit` from `on_exit` after the first join (slot empty).
    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    take_and_join_dx12_cache_validate_thread();

    // Force exit: the audio thread may hold CPAL/WASAPI resources whose
    // cleanup blocks indefinitely on Windows once the event loop is gone.
    // Settings are already persisted in on_exit(), so this is safe.
    if result.is_ok() {
        shutdown_logger();
        std::process::exit(0);
    }

    if let Err(e) = result {
        let app_ver = env!("CARGO_PKG_VERSION");
        let error_msg = format!(
            "Simple Image Viewer v{}\nEnvironment: {}\n\n{}: {}",
            app_ver,
            env_info,
            rust_i18n::t!("error.startup_failed"),
            e
        );

        log::error!("Application startup failed: {}", e);

        let help_hint = {
            #[cfg(target_os = "windows")]
            {
                let os_version = sysinfo::System::os_version().unwrap_or_default();
                if os_version.starts_with("6.1") {
                    // Windows 7
                    rust_i18n::t!("error.win7_graphics_hint").to_string()
                } else {
                    String::new()
                }
            }
            #[cfg(not(target_os = "windows"))]
            String::new()
        };

        // Try to copy to clipboard
        use clipboard_rs::{Clipboard, ClipboardContext};
        if let Ok(ctx) = ClipboardContext::new() {
            let _ = ctx.set_text(error_msg.clone());
        }

        let dialog_msg = if help_hint.is_empty() {
            format!(
                "{}\n\n{}",
                error_msg,
                rust_i18n::t!("error.copied_to_clipboard")
            )
        } else {
            format!(
                "{}\n\n{}\n\n{}",
                error_msg,
                rust_i18n::t!("error.copied_to_clipboard"),
                help_hint
            )
        };

        // No `set_parent`: `run_native` failed before a main window was created.
        rfd::MessageDialog::new()
            .set_description(&dialog_msg)
            .set_level(rfd::MessageLevel::Error)
            .show();

        shutdown_logger();
        return Err(e);
    }

    shutdown_logger();
    Ok(())
}
