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
use std::time::Instant;
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
mod mmap_util;
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

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
mod wgpu_preprobe_cache;

#[cfg(target_os = "windows")]
mod windows_utils;

/// Title-bar icon. On Windows this is decoded from the same PE `.ico` resource (id 1) used for
/// the taskbar; elsewhere `build.rs` embeds `OUT_DIR/siv_window_icon_rgba256.bin` from `icon.png`.
fn load_icon() -> egui::IconData {
    #[cfg(windows)]
    {
        if let Some((rgba, width, height)) = windows_utils::load_icon_rgba_from_pe() {
            return egui::IconData {
                rgba,
                width,
                height,
            };
        }
        log::warn!(
            "Failed to load application icon from PE resources; title bar may show a generic icon"
        );
        return egui::IconData {
            rgba: Vec::new(),
            width: 0,
            height: 0,
        };
    }
    #[cfg(not(windows))]
    {
        return load_icon_from_build_rgba();
    }
}

/// 256×256 RGBA from `build.rs` (`emit_viewport_icon_rgba`); Linux/macOS only.
#[cfg(not(windows))]
fn load_icon_from_build_rgba() -> egui::IconData {
    const W: u32 = 256;
    const H: u32 = 256;
    let rgba = include_bytes!(concat!(env!("OUT_DIR"), "/siv_window_icon_rgba256.bin"));
    debug_assert_eq!(rgba.len(), (W * H * 4) as usize);
    egui::IconData {
        rgba: rgba.to_vec(),
        width: W,
        height: H,
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
/// Decoder paths that use `catch_exr_panic` increment a thread-local so this hook skips
/// dialog/exit — otherwise `process::exit(1)` would run before `catch_unwind` can handle the panic.
fn setup_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        if crate::hdr::exr_tiled::is_exr_panic_hook_suppressed() {
            return;
        }

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

        // No `set_parent`: the crash hook can run when no egui window exists.
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

/// Field-diagnostic timings for cold start (look for `[startup]` in logs).
fn startup_log_phase(prev: &mut Instant, t0: Instant, label: &'static str) {
    let now = Instant::now();
    log::info!(
        "[startup] {:42} +{:5} ms   total {:6} ms",
        label,
        now.duration_since(*prev).as_millis(),
        now.duration_since(t0).as_millis()
    );
    *prev = now;
}

/// Backends used for Windows wgpu adapter enumeration at startup.
///
/// On **Windows ARM64**, `Backends::all()` also enables GLES/WGL; `wgpu_hal` then calls
/// `glow::get_parameter_indexed_string`, which can pass null into `strlen` and crash (WoA / VM).
/// Use `PRIMARY` (DX12 + Vulkan) only — same as normal desktop Windows without the GL fallback.
#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
fn windows_wgpu_probe_backends() -> eframe::wgpu::Backends {
    if let Some(backends) = eframe::wgpu::Backends::from_env() {
        return backends;
    }
    #[cfg(target_arch = "aarch64")]
    {
        eframe::wgpu::Backends::PRIMARY
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        eframe::wgpu::Backends::all()
    }
}

/// Default instance backends for [`eframe::egui_wgpu::WgpuSetupCreateNew`] on Windows ARM64.
#[cfg(all(
    target_os = "windows",
    target_arch = "aarch64",
    not(feature = "legacy_win7")
))]
fn apply_windows_arm64_default_wgpu_backends(
    wgpu_setup: &mut eframe::egui_wgpu::WgpuSetupCreateNew,
) {
    if eframe::wgpu::Backends::from_env().is_some() {
        return;
    }
    wgpu_setup.instance_descriptor.backends = eframe::wgpu::Backends::PRIMARY;
    log::info!(
        "[startup] Windows ARM64: wgpu backends {:?} (OpenGL/WGL disabled)",
        wgpu_setup.instance_descriptor.backends
    );
}

/// Result of the Windows-only wgpu adapter pre-probe (see `spawn_dx12_preprobe_thread`).
#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
#[derive(Clone, Copy)]
struct Dx12PreprobeOutcome {
    has_real_dx12: bool,
    enumerate_ms: u128,
    adapter_count: usize,
}

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
fn dx12_preprobe_outcome() -> Dx12PreprobeOutcome {
    let wgpu_probe_start = Instant::now();
    let instance =
        eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor::new_without_display_handle());
    let probe_backends = windows_wgpu_probe_backends();
    log::info!(
        "[startup] wgpu dx12 preprobe: enumerate backends {:?}",
        probe_backends
    );
    let adapters = pollster::block_on(instance.enumerate_adapters(probe_backends));
    let enumerate_ms = wgpu_probe_start.elapsed().as_millis() as u128;

    let has_real_dx12 = adapters.iter().any(|a| {
        let info = a.get_info();
        info.backend == eframe::wgpu::Backend::Dx12
            && matches!(
                info.device_type,
                eframe::wgpu::DeviceType::DiscreteGpu | eframe::wgpu::DeviceType::IntegratedGpu
            )
    });

    Dx12PreprobeOutcome {
        has_real_dx12,
        enumerate_ms,
        adapter_count: adapters.len(),
    }
}

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
fn apply_dx12_preprobe_to_wgpu_setup(
    wgpu_setup: &mut eframe::egui_wgpu::WgpuSetupCreateNew,
    force_dx12: bool,
    from_yaml_cache: bool,
) {
    if force_dx12 {
        if from_yaml_cache {
            log::info!(
                "[startup] wgpu preprobe cache: force_dx12=true — DX12 + HighPerformance (edit siv_wgpu_preprobe_cache.yaml if wrong)"
            );
        } else {
            log::info!(
                "Detected DX12 compatible hardware (Discrete/Integrated). Forcing DX12 backend."
            );
        }
        wgpu_setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        wgpu_setup.power_preference = eframe::wgpu::PowerPreference::HighPerformance;
    } else if from_yaml_cache {
        log::info!(
            "[startup] wgpu preprobe cache: force_dx12=false — default backend selection (edit siv_wgpu_preprobe_cache.yaml if wrong)"
        );
    } else {
        log::info!(
            "No real DX12 GPU found (only CPU, Virtual, or Other available). Falling back to default selection."
        );
    }
}

/// Runs [`dx12_preprobe_outcome`] on a dedicated thread and sends the result to the main thread.
/// Used when no yaml cache exists — the main thread must [`std::sync::mpsc::Receiver::recv`]
/// before [`eframe::run_native`] to apply backends.
#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
fn spawn_dx12_preprobe_thread() -> std::sync::mpsc::Receiver<Option<Dx12PreprobeOutcome>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let spawn_res = std::thread::Builder::new()
        .name("wgpu-dx12-preprobe".into())
        .spawn(move || {
            let to_send =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(dx12_preprobe_outcome)) {
                    Ok(o) => Some(o),
                    Err(_) => {
                        log::error!(
                            "[startup] wgpu dx12 preprobe panicked; using default backends, not updating cache"
                        );
                        None
                    }
                };
            if tx.send(to_send).is_err() {
                log::warn!("[startup] wgpu dx12 preprobe: main thread receiver dropped");
            }
        });
    if let Err(e) = spawn_res {
        log::error!(
            "[startup] Failed to spawn wgpu-dx12-preprobe thread ({}); running probe on main thread",
            e
        );
        let to_send =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(dx12_preprobe_outcome)) {
                Ok(o) => Some(o),
                Err(_) => {
                    log::error!(
                        "[startup] wgpu dx12 preprobe (main thread) panicked; not updating cache"
                    );
                    None
                }
            };
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let _ = tx.send(to_send);
        return rx;
    }
    rx
}

/// When yaml cache was applied on the main thread, re-probe in the background without blocking.
/// If the live result disagrees with the cache, rewrite yaml so the **next** launch matches hardware
/// (this session keeps the optimistic cache-backed `WgpuSetup`).
#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
fn spawn_dx12_cache_validate_thread(
    cached_force_dx12: bool,
) -> Option<std::thread::JoinHandle<()>> {
    let path = wgpu_preprobe_cache::cache_path();
    let spawn_res = std::thread::Builder::new()
        .name("wgpu-dx12-cache-validate".into())
        .spawn(move || {
            let outcome =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(dx12_preprobe_outcome)) {
                    Ok(o) => o,
                    Err(_) => {
                        log::error!(
                            "[startup] wgpu dx12 cache-validate panicked; leaving yaml unchanged"
                        );
                        return;
                    }
                };
            if outcome.has_real_dx12 != cached_force_dx12 {
                log::warn!(
                    "[startup] wgpu preprobe: background validate found stale yaml (cached force_dx12={} vs probe {}); rewriting {} for next launch (current session unchanged)",
                    cached_force_dx12,
                    outcome.has_real_dx12,
                    path.display(),
                );
                if let Err(e) = wgpu_preprobe_cache::save(outcome.has_real_dx12) {
                    log::warn!(
                        "[startup] failed to save wgpu preprobe cache {}: {}",
                        path.display(),
                        e
                    );
                }
            } else {
                log::info!(
                    "[startup] wgpu preprobe: background validate agrees with yaml (force_dx12={}, {} ms, {} adapters)",
                    outcome.has_real_dx12,
                    outcome.enumerate_ms,
                    outcome.adapter_count,
                );
            }
        });
    match spawn_res {
        Ok(h) => Some(h),
        Err(e) => {
            log::error!(
                "[startup] Failed to spawn wgpu-dx12-cache-validate thread: {}",
                e
            );
            None
        }
    }
}

/// Join a validate-thread handle (used from [`take_and_join_dx12_cache_validate_thread`] on exit).
#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
fn join_dx12_cache_validate_thread(jh: Option<std::thread::JoinHandle<()>>) {
    if let Some(h) = jh {
        if let Err(e) = h.join() {
            log::warn!(
                "[on_exit] wgpu-dx12-cache-validate thread panicked: {:?}",
                e
            );
        }
    }
}

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
static DX12_CACHE_VALIDATE_JOIN_ON_EXIT: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(None);

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
pub(crate) fn register_dx12_cache_validate_join_for_exit(handle: std::thread::JoinHandle<()>) {
    let mut slot = DX12_CACHE_VALIDATE_JOIN_ON_EXIT
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if slot.replace(handle).is_some() {
        log::warn!("[startup] wgpu dx12 cache-validate join slot overwritten");
    }
}

/// Called from [`ImageViewerApp::on_exit`] before `process::exit` on Windows so the validate
/// thread can finish writing `siv_wgpu_preprobe_cache.yaml` (see `join_dx12_cache_validate_thread`).
#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
pub(crate) fn take_and_join_dx12_cache_validate_thread() {
    let h = DX12_CACHE_VALIDATE_JOIN_ON_EXIT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    join_dx12_cache_validate_thread(h);
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
    let startup_t0 = Instant::now();
    let mut prev = startup_t0;

    // Install the Windows SEH exception filter as early as possible.
    // This catches native crashes (ACCESS_VIOLATION, STACK_OVERFLOW, etc.)
    // that bypass Rust's panic mechanism and would otherwise cause a
    // silent exit with no diagnostic output.
    #[cfg(target_os = "windows")]
    seh_handler::install();

    let mut settings = settings::Settings::load();
    init_logging(&settings);
    startup_log_phase(&mut prev, startup_t0, "seh + Settings::load + init_logging");

    let env_info = log_env_info();
    startup_log_phase(&mut prev, startup_t0, "log_env_info");

    hdr::tiled::configure_hdr_tile_cache_budget_from_system_memory();
    startup_log_phase(&mut prev, startup_t0, "hdr_tile_cache_budget");

    #[cfg(target_os = "windows")]
    {
        wic::init_rayon_with_com();
        wic::spawn_wic_discovery();
    }
    startup_log_phase(
        &mut prev,
        startup_t0,
        "wic_init_rayon + spawn_wic_discovery",
    );

    // Initialize locale — detect from OS if not yet configured
    if settings.language.is_empty() {
        settings.language = settings::detect_system_language();
    }
    rust_i18n::set_locale(&settings.language);

    // NOW setup the panic hook - with logging AND correct language ready
    setup_panic_hook();
    startup_log_phase(&mut prev, startup_t0, "locale + set_locale + panic_hook");

    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    let cached_preprobe: Option<wgpu_preprobe_cache::WgpuPreprobeCache> =
        match wgpu_preprobe_cache::load() {
            Some(cache) if cache.format_version == wgpu_preprobe_cache::FORMAT_VERSION => {
                Some(cache)
            }
            Some(cache) => {
                log::warn!(
                    "[startup] wgpu preprobe cache has unsupported format_version {} in {}; ignoring",
                    cache.format_version,
                    wgpu_preprobe_cache::cache_path().display(),
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

    let saved_inner_size = settings
        .window_inner_size
        .map(|[w, h]| [w as f32, h as f32])
        .unwrap_or([1280.0, 800.0]);
    let saved_outer_position = settings
        .window_outer_position
        .map(|[x, y]| [x as f32, y as f32]);
    let saved_maximized = settings.window_maximized;

    let app_icon = load_icon();
    startup_log_phase(&mut prev, startup_t0, "load_icon");

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(rust_i18n::t!("app.title").to_string())
        .with_inner_size(saved_inner_size)
        .with_min_inner_size([400.0, 300.0])
        .with_decorations(true)
        .with_fullscreen(fullscreen)
        .with_maximized(saved_maximized)
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
        let adapter_limits = adapter.limits();
        log::info!("GPU max_texture_dimension_2d: {}", hw_max_texture);
        log::info!(
            "GPU max_storage_buffer_binding_size: {}",
            adapter_limits.max_storage_buffer_binding_size
        );

        eframe::wgpu::DeviceDescriptor {
            label: Some("egui wgpu device"),
            required_limits: eframe::wgpu::Limits {
                max_texture_dimension_2d: hw_max_texture,
                max_storage_buffer_binding_size: adapter_limits.max_storage_buffer_binding_size,
                max_buffer_size: adapter_limits.max_buffer_size,
                ..base_limits
            },
            ..Default::default()
        }
    });

    startup_log_phase(&mut prev, startup_t0, "wgpu_setup_body (device_descriptor)");

    #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
    if let Some(ref cache) = cached_preprobe {
        apply_dx12_preprobe_to_wgpu_setup(&mut wgpu_setup, cache.force_dx12, true);
        log::info!(
            "[startup] wgpu preprobe: applied cache {} (main thread will not wait; background thread validates)",
            wgpu_preprobe_cache::cache_path().display()
        );
        startup_log_phase(
            &mut prev,
            startup_t0,
            "wgpu dx12 preprobe (yaml cache; no recv wait)",
        );
    }

    let hdr_spawn_start = Instant::now();
    let (preferred_hdr_target_format, hdr_environment_probe) =
        crate::hdr::surface::preferred_native_hdr_target_format_for_environment(
            settings.hdr_native_surface_enabled_effective(),
            settings.window_outer_position,
        );
    log::info!(
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
    for diagnostic in crate::hdr::surface::native_hdr_surface_request_diagnostics(
        settings.hdr_native_surface_enabled_effective(),
        preferred_hdr_target_format,
    ) {
        log::info!("{diagnostic}");
    }
    log::info!("[HDR] environment_probe={hdr_environment_probe:?}");

    #[cfg(target_os = "linux")]
    {
        log::info!(
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
        let recv_wait = Instant::now();
        let maybe_outcome = dx12_preprobe_rx
            .recv()
            .expect("wgpu dx12 preprobe thread exited without sending a result");
        let main_wait_ms = recv_wait.elapsed().as_millis();

        if let Some(outcome) = maybe_outcome {
            let probe_force = outcome.has_real_dx12;
            apply_dx12_preprobe_to_wgpu_setup(&mut wgpu_setup, probe_force, false);
            if let Err(e) = wgpu_preprobe_cache::save(probe_force) {
                log::warn!(
                    "[startup] failed to save wgpu preprobe cache {}: {}",
                    wgpu_preprobe_cache::cache_path().display(),
                    e
                );
            } else {
                log::info!(
                    "[startup] wgpu preprobe: wrote cache {}",
                    wgpu_preprobe_cache::cache_path().display()
                );
            }
            log::info!(
                "[startup] wgpu pre-probe enumerate_adapters: {} ms (adapter count {}); main recv wait: {} ms",
                outcome.enumerate_ms,
                outcome.adapter_count,
                main_wait_ms
            );
        } else {
            log::error!(
                "[startup] wgpu dx12 preprobe failed; using default wgpu backends, cache file unchanged ({})",
                wgpu_preprobe_cache::cache_path().display()
            );
        }
        startup_log_phase(&mut prev, startup_t0, "wgpu dx12 preprobe recv + apply");
    }

    let native_options = eframe::NativeOptions {
        viewport,
        centered: center_window_on_open,
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
    seh_handler::reinstall_top_level_filter();

    log::info!(
        "[startup] Main-thread prep before window/event loop: {} ms total",
        prev.duration_since(startup_t0).as_millis()
    );

    let result = eframe::run_native(
        "Simple Image Viewer",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::ImageViewerApp::new(
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

        return Err(e);
    }

    Ok(())
}
