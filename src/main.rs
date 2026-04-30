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

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Use mimalloc for all platforms — the default Windows HeapAlloc has severe
// lock contention when many threads concurrently allocate/free ~68KB buffers
// (one per PSB row decode). mimalloc uses per-thread heaps to eliminate this.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

rust_i18n::i18n!("locales");

mod app;
use eframe::egui;
mod audio;
mod constants;
mod formats;
mod hdr;
mod ipc;
mod libtiff_loader;
mod loader;
#[cfg(target_os = "macos")]
mod macos_image_io;
mod metadata_utils;
pub mod print;
mod psb_reader;
mod raw_processor;
mod scanner;
#[cfg(target_os = "windows")]
mod seh_handler;
mod settings;
mod simd_swizzle;
pub mod theme;
mod tile_cache;
mod ui;
#[cfg(target_os = "windows")]
mod wic;

#[cfg(target_os = "windows")]
mod windows_utils;

/// Load the application icon from the embedded PNG bytes.
fn load_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/icon.png");
    match image::load_from_memory(bytes) {
        Ok(img) => {
            use image::GenericImageView;
            use image::imageops::FilterType;
            let img = img.resize_exact(256, 256, FilterType::Lanczos3);
            let (w, h) = img.dimensions();
            egui::IconData {
                rgba: img.to_rgba8().into_raw(),
                width: w,
                height: h,
            }
        }
        Err(e) => {
            eprintln!("Failed to load app icon: {e}");
            egui::IconData::default()
        }
    }
}

fn init_logging(settings: &crate::settings::Settings) {
    let log_dir = crate::settings::settings_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let logger = flexi_logger::Logger::try_with_env_or_str(&settings.log_level)
        .expect("Failed to initialize logger");

    let logger = if settings.enable_log_file {
        logger.log_to_file(
            flexi_logger::FileSpec::default()
                .directory(log_dir)
                .basename("simple_image_viewer"),
        )
    } else {
        logger
    };

    #[cfg(windows)]
    let logger = logger.use_windows_line_ending();

    let logger = logger
        .write_mode(flexi_logger::WriteMode::BufferAndFlush)
        .rotate(
            flexi_logger::Criterion::Size(crate::constants::LOG_FILE_SIZE_LIMIT),
            flexi_logger::Naming::Numbers,
            flexi_logger::Cleanup::KeepLogFiles(3),
        );

    #[cfg(debug_assertions)]
    let logger = logger.duplicate_to_stderr(flexi_logger::Duplicate::All);

    // Start the logger. The returned handle can be dropped as we don't
    // need to reconfigure the logger dynamically.
    let _ = logger.start();
}

fn log_env_info() -> String {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();

    let total_memory = sys.total_memory();
    let memory_gb = total_memory as f64 / 1024.0 / 1024.0 / 1024.0;

    #[cfg(windows)]
    let env_desc = {
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        use windows::core::PCSTR;

        #[repr(C)]
        #[allow(non_snake_case)]
        struct OSVERSIONINFOEXW {
            dwOSVersionInfoSize: u32,
            dwMajorVersion: u32,
            dwMinorVersion: u32,
            dwBuildNumber: u32,
            dwPlatformId: u32,
            szCSDVersion: [u16; 128],
            wServicePackMajor: u16,
            wServicePackMinor: u16,
            wSuiteMask: u16,
            wProductType: u8,
            wReserved: u8,
        }

        unsafe fn get_win_env(memory_gb: f64) -> Option<String> {
            let h_ntdll = unsafe { GetModuleHandleW(windows::core::w!("ntdll.dll")).ok()? };
            let proc = unsafe { GetProcAddress(h_ntdll, PCSTR(b"RtlGetVersion\0".as_ptr()))? };
            let rtl_get_version: extern "system" fn(*mut OSVERSIONINFOEXW) -> i32 =
                unsafe { std::mem::transmute(proc) };

            let mut osi: OSVERSIONINFOEXW = unsafe { std::mem::zeroed() };
            osi.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOEXW>() as u32;

            if rtl_get_version(&mut osi) == 0 {
                let major = osi.dwMajorVersion;
                let minor = osi.dwMinorVersion;
                let build = osi.dwBuildNumber;
                let is_server = osi.wProductType != 1;

                let service_pack = String::from_utf16_lossy(&osi.szCSDVersion);
                let service_pack = service_pack.trim_matches('\0').trim().to_string();

                use winreg::RegKey;
                use winreg::enums::HKEY_LOCAL_MACHINE;
                let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

                let marketing_name = match (major, minor) {
                    (10, 0) => {
                        if is_server {
                            if build >= 26100 {
                                "Server 2025"
                            } else if build >= 20348 {
                                "Server 2022"
                            } else if build >= 17763 {
                                "Server 2019"
                            } else if build >= 14393 {
                                "Server 2016"
                            } else {
                                "Server"
                            }
                        } else {
                            if build >= 22000 { "11" } else { "10" }
                        }
                    }
                    (6, 3) => {
                        if is_server {
                            "Server 2012 R2"
                        } else {
                            "8.1"
                        }
                    }
                    (6, 2) => {
                        if is_server {
                            "Server 2012"
                        } else {
                            "8"
                        }
                    }
                    (6, 1) => {
                        if is_server {
                            "Server 2008 R2"
                        } else {
                            "7"
                        }
                    }
                    (6, 0) => {
                        if is_server {
                            "Server 2008"
                        } else {
                            "Vista"
                        }
                    }
                    (5, 2) => {
                        if is_server {
                            "Server 2003"
                        } else {
                            "XP"
                        }
                    }
                    (5, 1) => "XP",
                    _ => "Unknown",
                };

                let mut display_name = format!("Windows {}", marketing_name);
                let mut display_version = String::new();
                let mut edition_id = String::new();
                let mut ubr: u32 = 0;

                if let Ok(key) = hklm.open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") {
                    display_version = key
                        .get_value("DisplayVersion")
                        .or_else(|_| key.get_value("ReleaseId"))
                        .unwrap_or_default();
                    edition_id = key.get_value("EditionID").unwrap_or_default();
                    ubr = key.get_value("UBR").unwrap_or(0);
                }

                if !edition_id.is_empty() {
                    display_name.push_str(" ");
                    display_name.push_str(&edition_id);
                }
                if !display_version.is_empty() {
                    display_name.push_str(" ");
                    display_name.push_str(&display_version);
                }
                if !service_pack.is_empty() {
                    display_name.push_str(" ");
                    display_name.push_str(&service_pack);
                }

                let full_version = if ubr > 0 {
                    format!("{}.{}.{}.{}", major, minor, build, ubr)
                } else {
                    format!("{}.{}.{}", major, minor, build)
                };

                return Some(format!(
                    "{} [{}] (RAM: {:.2} GB)",
                    display_name, full_version, memory_gb
                ));
            }
            None
        }

        unsafe { get_win_env(memory_gb) }
    };

    #[cfg(not(windows))]
    let env_desc: Option<String> = None;

    let final_desc = env_desc.unwrap_or_else(|| {
        let os_name = sysinfo::System::name().unwrap_or_else(|| "Unknown".to_string());
        let os_version = sysinfo::System::os_version().unwrap_or_else(|| "Unknown".to_string());
        format!("{} [{}] (RAM: {:.2} GB)", os_name, os_version, memory_gb)
    });

    log::info!(
        "Simple Image Viewer v{} | Environment: {}",
        env!("CARGO_PKG_VERSION"),
        final_desc
    );

    #[cfg(feature = "legacy_win7")]
    log::info!("Build Type: Windows 7 Legacy Compatibility Edition (x64)");

    final_desc
}

/// Set up a global panic hook to capture and report crashes across all threads.
fn setup_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_string());
        let payload = panic_info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "no message".to_string()
        };

        let app_ver = env!("CARGO_PKG_VERSION");

        // Capture a full backtrace
        let backtrace = std::backtrace::Backtrace::force_capture();

        // Re-detect basic env info for the report
        let os_name = sysinfo::System::name().unwrap_or_else(|| "Unknown OS".to_string());
        let os_ver = sysinfo::System::os_version().unwrap_or_else(|| "Unknown Version".to_string());

        let report = format!(
            "--- Simple Image Viewer Crash Report ---\n\
            Version: v{}\n\
            OS: {} [{}]\n\
            Location: {}\n\
            Error: {}\n\n\
            STACK BACKTRACE:\n\
            {:?}\n\
            ----------------------------------------\n",
            app_ver, os_name, os_ver, location, message, backtrace
        );

        // 1. Log to stderr (for console users) and file system
        eprintln!("{}", report);
        log::error!("{}", report);

        let log_path = crate::settings::settings_path()
            .with_file_name(crate::constants::CRASH_REPORT_FILENAME);
        let _ = std::fs::write(&log_path, &report);

        // 2. Try to copy to clipboard
        use clipboard_rs::{Clipboard, ClipboardContext};
        if let Ok(ctx) = ClipboardContext::new() {
            let _ = ctx.set_text(report.clone());
        }

        // 3. Show localized error dialog (if i18n is available, else fallback to English)
        let mut title = rust_i18n::t!("dialog.crash_title").to_string();
        if title.contains("dialog.crash_title") {
            title = crate::constants::CRASH_DIALOG_FALLBACK_TITLE.to_string();
        }

        let mut msg = format!(
            "{}\n\n{}\n\n{}",
            rust_i18n::t!("dialog.crash_msg"),
            format!("Location: {}", location),
            format!("Error: {}", message)
        );
        if msg.contains("dialog.crash_msg") {
            msg = format!(
                "{}\n\nLocation: {}\nError: {}\n\nDiagnostic info copied to clipboard.",
                crate::constants::CRASH_DIALOG_FALLBACK_MSG,
                location,
                message
            );
        }

        // Use rfd for a system native dialog
        rfd::MessageDialog::new()
            .set_title(&title)
            .set_description(&msg)
            .set_level(rfd::MessageLevel::Error)
            .show();

        // Critical: After showing the crash dialog, the application must terminate.
        // Otherwise, the window may hang or enter an unstable state.
        std::process::exit(1);
    }));
}

fn main() -> eframe::Result {
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
    if ipc::setup_or_forward_args(ipc_tx, initial_image.as_ref(), no_recursive) {
        // We Successfully forwarded to another instance, exit.
        std::process::exit(0);
    }

    // 3. Primary Instance Initialization

    // Install the Windows SEH exception filter as early as possible.
    // This catches native crashes (ACCESS_VIOLATION, STACK_OVERFLOW, etc.)
    // that bypass Rust's panic mechanism and would otherwise cause a
    // silent exit with no diagnostic output.
    #[cfg(target_os = "windows")]
    seh_handler::install();

    let mut settings = settings::Settings::load();
    init_logging(&settings);
    let env_info = log_env_info();

    #[cfg(target_os = "windows")]
    {
        wic::init_rayon_with_com();
        wic::spawn_wic_discovery();
    }

    // Initialize locale — detect from OS if not yet configured
    if settings.language.is_empty() {
        settings.language = settings::detect_system_language();
    }
    rust_i18n::set_locale(&settings.language);

    // NOW setup the panic hook - with logging AND correct language ready
    setup_panic_hook();

    // Apply command-line overrides to settings
    if let Some(ref path) = initial_image {
        if let Some(parent) = path.parent() {
            settings.last_image_dir = Some(parent.to_path_buf());
        }
        settings.auto_switch = false;
        settings.recursive = false;
    }

    let fullscreen = settings.fullscreen;

    let viewport = egui::ViewportBuilder::default()
        .with_title(rust_i18n::t!("app.title").to_string())
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([400.0, 300.0])
        .with_decorations(true)
        .with_fullscreen(fullscreen)
        .with_icon(load_icon());

    let mut wgpu_setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    wgpu_setup.device_descriptor = std::sync::Arc::new(|adapter| {
        let info = adapter.get_info();
        log::info!("Graphics Adapter Info: {} ({:?})", info.name, info.backend);
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
        log::info!("GPU max_texture_dimension_2d: {}", hw_max_texture);

        eframe::wgpu::DeviceDescriptor {
            label: Some("egui wgpu device"),
            required_limits: eframe::wgpu::Limits {
                max_texture_dimension_2d: hw_max_texture,
                ..base_limits
            },
            ..Default::default()
        }
    });

    // Explicit hardware probing to prioritize DX12 on modern Windows
    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    {
        // Use a temporary instance to probe adapter capabilities
        let instance = eframe::wgpu::Instance::new(
            eframe::wgpu::InstanceDescriptor::new_without_display_handle(),
        );
        let adapters =
            pollster::block_on(instance.enumerate_adapters(eframe::wgpu::Backends::all()));

        let has_real_dx12 = adapters.iter().any(|a| {
            let info = a.get_info();
            info.backend == eframe::wgpu::Backend::Dx12
                && matches!(
                    info.device_type,
                    eframe::wgpu::DeviceType::DiscreteGpu | eframe::wgpu::DeviceType::IntegratedGpu
                )
        });

        if has_real_dx12 {
            log::info!(
                "Detected DX12 compatible hardware (Discrete/Integrated). Forcing DX12 backend."
            );
            wgpu_setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
            wgpu_setup.power_preference = eframe::wgpu::PowerPreference::HighPerformance;
        } else {
            log::info!(
                "No real DX12 GPU found (only CPU, Virtual, or Other available). Falling back to default selection."
            );
        }
    }

    let preferred_hdr_target_format =
        crate::hdr::surface::preferred_native_hdr_target_format_for_settings(
            settings.hdr_native_surface_enabled,
        );

    let native_options = eframe::NativeOptions {
        viewport,
        centered: true,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(wgpu_setup),
            preferred_target_format: preferred_hdr_target_format,
            ..Default::default()
        },
        // Dithering assumes SDR gamma-space output. Leave it off when we ask
        // for a float HDR target; egui-wgpu falls back safely if unsupported.
        dithering: preferred_hdr_target_format.is_none(),
        ..Default::default()
    };

    let result = eframe::run_native(
        "Simple Image Viewer",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::ImageViewerApp::new(
                cc,
                settings,
                initial_image,
                ipc_rx,
            )) as Box<dyn eframe::App>)
        }),
    );

    // Force exit: the audio thread may hold CPAL/WASAPI resources whose
    // cleanup blocks indefinitely on Windows once the event loop is gone.
    // Settings are already persisted in on_exit(), so this is safe.
    if result.is_ok() {
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

        rfd::MessageDialog::new()
            .set_title(rust_i18n::t!("dialog.startup_error_title").to_string())
            .set_description(&dialog_msg)
            .set_level(rfd::MessageLevel::Error)
            .show();

        return Err(e);
    }

    Ok(())
}
