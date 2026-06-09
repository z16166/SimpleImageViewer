use parking_lot::Mutex;
use std::time::Instant;

/// Backends used for Windows wgpu adapter enumeration at startup.
///
/// On **Windows ARM64**, `Backends::all()` also enables GLES/WGL; `wgpu_hal` then calls
/// `glow::get_parameter_indexed_string`, which can pass null into `strlen` and crash (WoA / VM).
/// Use `PRIMARY` (DX12 + Vulkan) only — same as normal desktop Windows without the GL fallback.
pub fn windows_wgpu_probe_backends() -> eframe::wgpu::Backends {
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
#[cfg(target_arch = "aarch64")]
pub fn apply_windows_arm64_default_wgpu_backends(
    wgpu_setup: &mut eframe::egui_wgpu::WgpuSetupCreateNew,
) {
    if eframe::wgpu::Backends::from_env().is_some() {
        return;
    }
    wgpu_setup.instance_descriptor.backends = eframe::wgpu::Backends::PRIMARY;
    crate::startup_info!(
        "[startup] Windows ARM64: wgpu backends {:?} (OpenGL/WGL disabled)",
        wgpu_setup.instance_descriptor.backends
    );
}

/// Result of the Windows-only wgpu adapter pre-probe (see `spawn_dx12_preprobe_thread`).
#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "startup-timing"), allow(dead_code))]
pub struct Dx12PreprobeOutcome {
    pub has_real_dx12: bool,
    pub enumerate_ms: u128,
    pub adapter_count: usize,
}

pub fn dx12_preprobe_outcome() -> Dx12PreprobeOutcome {
    let wgpu_probe_start = Instant::now();
    let instance =
        eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor::new_without_display_handle());
    let probe_backends = windows_wgpu_probe_backends();
    crate::startup_info!(
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

pub fn apply_dx12_preprobe_to_wgpu_setup(
    wgpu_setup: &mut eframe::egui_wgpu::WgpuSetupCreateNew,
    force_dx12: bool,
    from_yaml_cache: bool,
) {
    if force_dx12 {
        if from_yaml_cache {
            crate::startup_info!(
                "[startup] wgpu preprobe cache: force_dx12=true — DX12 + HighPerformance (edit siv_wgpu_preprobe_cache.yaml if wrong)"
            );
        } else {
            crate::startup_info!(
                "Detected DX12 compatible hardware (Discrete/Integrated). Forcing DX12 backend."
            );
        }
        wgpu_setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        wgpu_setup.power_preference = eframe::wgpu::PowerPreference::HighPerformance;
    } else if from_yaml_cache {
        crate::startup_info!(
            "[startup] wgpu preprobe cache: force_dx12=false — default backend selection (edit siv_wgpu_preprobe_cache.yaml if wrong)"
        );
    } else {
        crate::startup_info!(
            "No real DX12 GPU found (only CPU, Virtual, or Other available). Falling back to default selection."
        );
    }
}

/// Runs [`dx12_preprobe_outcome`] on a dedicated thread and sends the result to the main thread.
/// Used when no yaml cache exists — the main thread must [`std::sync::mpsc::Receiver::recv`]
/// before [`eframe::run_native`] to apply backends.
pub fn spawn_dx12_preprobe_thread() -> std::sync::mpsc::Receiver<Option<Dx12PreprobeOutcome>> {
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
pub fn spawn_dx12_cache_validate_thread(
    cached_force_dx12: bool,
) -> Option<std::thread::JoinHandle<()>> {
    let path = crate::wgpu_preprobe_cache::cache_path();
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
                if let Err(e) = crate::wgpu_preprobe_cache::save(outcome.has_real_dx12) {
                    log::warn!(
                        "[startup] failed to save wgpu preprobe cache {}: {}",
                        path.display(),
                        e
                    );
                }
            } else {
                crate::startup_info!(
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

static DX12_CACHE_VALIDATE_JOIN_ON_EXIT: Mutex<Option<std::thread::JoinHandle<()>>> =
    Mutex::new(None);

pub(crate) fn register_dx12_cache_validate_join_for_exit(handle: std::thread::JoinHandle<()>) {
    let mut slot = DX12_CACHE_VALIDATE_JOIN_ON_EXIT.lock();
    if slot.replace(handle).is_some() {
        log::warn!("[startup] wgpu dx12 cache-validate join slot overwritten");
    }
}

/// Called from [`ImageViewerApp::on_exit`] before `process::exit` on Windows so the validate
/// thread can finish writing `siv_wgpu_preprobe_cache.yaml` (see `join_dx12_cache_validate_thread`).
pub(crate) fn take_and_join_dx12_cache_validate_thread() {
    let h = DX12_CACHE_VALIDATE_JOIN_ON_EXIT.lock().take();
    join_dx12_cache_validate_thread(h);
}
