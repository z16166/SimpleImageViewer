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

//! On-disk persistence for [`wgpu::PipelineCache`] (DX12/Metal/Vulkan pipeline caches via patched wgpu-hal).
//!
//! Startup starts one background thread that deletes pipeline-cache files from older
//! builds. The file this process reads and writes is excluded. Shutdown joins that
//! thread before `process::exit` / `_exit`, which would otherwise abort the delete.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

/// Bump when wgpu render pipelines or WGSL change in ways that invalidate on-disk cache bytes.
const PIPELINE_CACHE_SCHEMA_VERSION: u32 = 12;

const PIPELINE_CACHE_CLEANUP_THREAD_NAME: &str = "wgpu-cache-cleanup";
const PIPELINE_CACHE_CLEANUP_JOIN_LABEL: &str = "wgpu pipeline cache cleanup";
/// Fallback stem written by [`cache_path_for_adapter_info`] for non-Vulkan adapters.
const PIPELINE_CACHE_FILE_PREFIX: &str = "siv_wgpu_pipeline_cache_";
/// Vulkan stem from `wgpu::util::pipeline_cache_key`, before our schema suffix.
const WGPU_PIPELINE_CACHE_FILE_PREFIX: &str = "wgpu_pipeline_cache_";
const PIPELINE_CACHE_VERSION_MARK: &str = "_pcv";

pub fn adapter_supports_pipeline_cache(adapter: &wgpu::Adapter) -> bool {
    adapter.features().contains(wgpu::Features::PIPELINE_CACHE)
}

/// Device features to request at creation time. Pipeline cache is optional so VMs and
/// older drivers can still create a device (pipelines compile without on-disk cache).
pub fn required_device_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    if adapter_supports_pipeline_cache(adapter) {
        wgpu::Features::PIPELINE_CACHE
    } else {
        wgpu::Features::empty()
    }
}

pub fn cache_path_for_adapter_info(info: &wgpu::AdapterInfo) -> PathBuf {
    let stem = wgpu::util::pipeline_cache_key(info).unwrap_or_else(|| {
        format!(
            "siv_wgpu_pipeline_cache_{:?}_{}_{}",
            info.backend, info.vendor, info.device
        )
    });
    let stem = format!(
        "{stem}_pcv{PIPELINE_CACHE_SCHEMA_VERSION}_drv{:016x}",
        driver_cache_hash(info)
    );
    cache_dir().join(format!("{stem}.bin"))
}

pub fn cache_path(adapter: &wgpu::Adapter) -> PathBuf {
    cache_path_for_adapter_info(&adapter.get_info())
}

fn cache_dir() -> PathBuf {
    crate::settings::settings_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn driver_cache_hash(info: &wgpu::AdapterInfo) -> u64 {
    let mut hash = stable_hash_bytes(info.driver.as_bytes(), 0xcbf2_9ce4_8422_2325);
    hash = stable_hash_bytes(info.driver_info.as_bytes(), hash);
    hash
}

fn stable_hash_bytes(bytes: &[u8], seed: u64) -> u64 {
    // FNV-1a 64-bit: fast, not collision-free. A driver-string hash collision would reuse
    // the wrong on-disk pipeline cache after a driver upgrade; worst case is bad rendering
    // until the user clears the cache directory (see `cache_path`).
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

pub fn load_for_adapter(adapter: &wgpu::Adapter) -> Option<Vec<u8>> {
    let path = cache_path(adapter);
    // A previous process::exit can kill the writer after it truncates this temp file
    // and before the rename. Drop that leftover before any new save starts.
    discard_incomplete_temp(&path);
    match std::fs::read(&path) {
        Ok(data) => {
            log::info!(
                "[HDR] loaded wgpu pipeline cache {} ({} bytes)",
                path.display(),
                data.len()
            );
            remember_saved(&path, &data);
            Some(data)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            log::warn!(
                "[HDR] ignoring unreadable wgpu pipeline cache {}: {error}",
                path.display()
            );
            None
        }
    }
}

/// Writes on-disk pipeline cache bytes synchronously (Windows non-DX12 path only).
#[cfg(target_os = "windows")]
fn persist(info: &wgpu::AdapterInfo, cache: &wgpu::PipelineCache) {
    let Some(data) = cache.get_data() else {
        return;
    };
    if let Err(error) = save_to_path(&cache_path_for_adapter_info(info), &data) {
        log::warn!("[HDR] failed to save wgpu pipeline cache: {error}");
    }
}

/// Same as [`persist`] but performs disk I/O on a background thread (for UI-thread call sites).
pub fn persist_async(info: &wgpu::AdapterInfo, cache: &wgpu::PipelineCache) {
    let path = cache_path_for_adapter_info(info);
    let Some(data) = cache.get_data() else {
        return;
    };
    spawn_persist(path, data);
}

/// Finish any in-flight save, then write `cache` on this thread.
///
/// Windows shutdown calls [`std::process::exit`] immediately after this. That
/// aborts background threads without flushing, so an async save leaves a 0-byte
/// `.tmp` beside the real cache. Callers that are about to exit must use this.
pub fn persist_blocking(info: &wgpu::AdapterInfo, cache: &wgpu::PipelineCache) {
    finish_persist_thread();
    let path = cache_path_for_adapter_info(info);
    let Some(data) = cache.get_data() else {
        return;
    };
    if let Err(error) = save_to_path(&path, &data) {
        log::warn!("[HDR] failed to save wgpu pipeline cache: {error}");
    }
}

fn persist_slot() -> &'static Mutex<Option<std::thread::JoinHandle<()>>> {
    static SLOT: OnceLock<Mutex<Option<std::thread::JoinHandle<()>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn spawn_persist(path: PathBuf, data: Vec<u8>) {
    if last_saved_matches(&path, &data) {
        return;
    }
    let mut slot = persist_slot().lock();
    if slot.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return;
    }
    if let Some(handle) = slot.take() {
        drop(slot);
        let _ = handle.join();
        slot = persist_slot().lock();
    }
    match std::thread::Builder::new()
        .name("siv-wgpu-pcache-persist".to_string())
        .spawn(move || {
            if let Err(error) = save_to_path(&path, &data) {
                log::warn!("[HDR] failed to save wgpu pipeline cache: {error}");
            }
        }) {
        Ok(handle) => *slot = Some(handle),
        Err(err) => log::warn!("[HDR] failed to spawn wgpu pipeline cache persist thread: {err}"),
    }
}

fn finish_persist_thread() {
    let handle = persist_slot().lock().take();
    if let Some(handle) = handle
        && handle.join().is_err()
    {
        log::warn!("[HDR] wgpu pipeline cache persist thread panicked");
    }
}

fn cleanup_join_slot() -> &'static Mutex<Option<std::thread::JoinHandle<()>>> {
    static SLOT: OnceLock<Mutex<Option<std::thread::JoinHandle<()>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Delete older on-disk pipeline caches on a background thread.
///
/// `protected_path` is the cache this process will read and write. That file and its
/// `.tmp` sibling are never deleted. Pass `None` when this session has no live cache
/// (no wgpu device, or the adapter cannot use `PipelineCache`): only files from older
/// schema versions are removed, so a later launch can still reuse a current-schema file.
///
/// The scan runs once per process. Call [`join_pipeline_cache_cleanup`] before
/// `process::exit` / `_exit`, which abort the thread without finishing the deletes.
pub(crate) fn start_pipeline_cache_cleanup(protected_path: Option<PathBuf>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let dir = protected_path
        .as_ref()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(cache_dir);
    match std::thread::Builder::new()
        .name(PIPELINE_CACHE_CLEANUP_THREAD_NAME.to_string())
        .spawn(move || remove_old_pipeline_cache_files(&dir, protected_path.as_deref()))
    {
        Ok(handle) => *cleanup_join_slot().lock() = Some(handle),
        Err(error) => {
            STARTED.store(false, Ordering::Release);
            log::warn!("[HDR] failed to spawn pipeline cache cleanup thread: {error}");
        }
    }
}

/// Join the cleanup thread started by [`start_pipeline_cache_cleanup`].
///
/// Safe to call more than once. A stuck delete (for example a disconnected network
/// volume) must not block process exit forever, so the wait uses the shared background
/// join budget and then detaches.
pub(crate) fn join_pipeline_cache_cleanup() {
    let Some(handle) = cleanup_join_slot().lock().take() else {
        return;
    };
    crate::app::background_threads::join_handle_with_timeout(
        handle,
        crate::app::background_threads::BACKGROUND_THREAD_JOIN_TIMEOUT,
        PIPELINE_CACHE_CLEANUP_JOIN_LABEL,
    );
}

fn protected_file_names(path: &Path) -> Vec<OsString> {
    let mut names = Vec::new();
    if let Some(name) = path.file_name() {
        names.push(name.to_os_string());
    }
    if let Some(temp_name) = temp_path_for(path).file_name() {
        names.push(temp_name.to_os_string());
    }
    names
}

fn is_protected_file_name(file_name: &OsStr, protected_names: &[OsString]) -> bool {
    protected_names.iter().any(|protected| {
        protected == file_name
            || protected
                .to_string_lossy()
                .eq_ignore_ascii_case(&file_name.to_string_lossy())
    })
}

fn pipeline_cache_artifact_stem(name: &str) -> Option<&str> {
    name.strip_suffix(".bin")
        .or_else(|| name.strip_suffix(".tmp"))
}

fn is_pipeline_cache_artifact(name: &str) -> bool {
    let Some(stem) = pipeline_cache_artifact_stem(name) else {
        return false;
    };
    stem.starts_with(PIPELINE_CACHE_FILE_PREFIX)
        || stem.starts_with(WGPU_PIPELINE_CACHE_FILE_PREFIX)
}

fn embedded_schema_version(name: &str) -> Option<u32> {
    let stem = pipeline_cache_artifact_stem(name).unwrap_or(name);
    let idx = stem.rfind(PIPELINE_CACHE_VERSION_MARK)?;
    let rest = &stem[idx + PIPELINE_CACHE_VERSION_MARK.len()..];
    let end = rest.find(|ch: char| !ch.is_ascii_digit())?;
    if end == 0 || !rest[end..].starts_with('_') {
        return None;
    }
    rest[..end].parse().ok()
}

fn adapter_cache_prefix(file_name: &str) -> Option<&str> {
    let stem = pipeline_cache_artifact_stem(file_name).unwrap_or(file_name);
    let idx = stem.rfind(PIPELINE_CACHE_VERSION_MARK)?;
    let prefix = &stem[..idx];
    if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    }
}

/// `base_prefix` must be the live file's name up to `_pcv`. The next characters have to
/// be that marker, so device id `10464` does not also match `104640`.
fn is_same_adapter_artifact(name: &str, base_prefix: &str) -> bool {
    name.strip_prefix(base_prefix)
        .is_some_and(|rest| rest.starts_with(PIPELINE_CACHE_VERSION_MARK))
}

fn should_remove_pipeline_cache_file(
    file_name: &OsStr,
    protected_names: &[OsString],
    live_prefix: Option<&str>,
) -> bool {
    if is_protected_file_name(file_name, protected_names) {
        return false;
    }
    let Some(name) = file_name.to_str() else {
        return false;
    };
    if !is_pipeline_cache_artifact(name) {
        return false;
    }
    match embedded_schema_version(name) {
        // Older builds. Invalid after a schema bump on every backend and GPU.
        Some(version) if version < PIPELINE_CACHE_SCHEMA_VERSION => true,
        // Same adapter, previous driver hash. The live file is already excluded above.
        Some(version) if version == PIPELINE_CACHE_SCHEMA_VERSION => {
            live_prefix.is_some_and(|prefix| is_same_adapter_artifact(name, prefix))
        }
        // Newer than this binary (downgrade). Leave it for the newer build.
        Some(_) => false,
        // Pre-schema layout (`siv_wgpu_pipeline_cache_*.bin` with no `_pcv`).
        None => true,
    }
}

fn remove_old_pipeline_cache_files(dir: &Path, protected_path: Option<&Path>) {
    let protected_names = protected_path.map(protected_file_names).unwrap_or_default();
    let live_name = protected_path.and_then(|path| path.file_name().and_then(|name| name.to_str()));
    let live_prefix_owned = live_name.and_then(adapter_cache_prefix).map(str::to_owned);
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            log::warn!(
                "[HDR] skipped pipeline cache cleanup in {}: {error}",
                dir.display()
            );
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Do not follow symlinks or walk subdirectories. Cache files sit next to the exe.
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        if !should_remove_pipeline_cache_file(
            &file_name,
            &protected_names,
            live_prefix_owned.as_deref(),
        ) {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                log::info!("[HDR] removed stale wgpu pipeline cache {}", path.display());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                log::warn!(
                    "[HDR] failed to remove stale wgpu pipeline cache {}: {error}",
                    path.display()
                );
            }
        }
    }
}

#[cfg(target_os = "windows")]
#[allow(dead_code)] // DX12 uses PipelineCache auto-persist; manual save for other adapters
pub fn save_atomic(adapter: &wgpu::Adapter, data: &[u8]) -> std::io::Result<()> {
    save_to_path(&cache_path(adapter), data)
}

fn last_saved() -> &'static Mutex<std::collections::HashMap<PathBuf, Vec<u8>>> {
    static SAVED: OnceLock<Mutex<std::collections::HashMap<PathBuf, Vec<u8>>>> = OnceLock::new();
    SAVED.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn last_saved_matches(path: &Path, data: &[u8]) -> bool {
    last_saved()
        .lock()
        .get(path)
        .is_some_and(|saved| saved == data)
}

fn remember_saved(path: &Path, data: &[u8]) {
    let mut saved = last_saved().lock();
    if saved.get(path).is_some_and(|existing| existing == data) {
        return;
    }
    saved.insert(path.to_path_buf(), data.to_vec());
}

fn temp_path_for(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}

fn discard_incomplete_temp(path: &Path) {
    let _ = std::fs::remove_file(temp_path_for(path));
}

fn save_to_path(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if last_saved_matches(path, data) {
        log::debug!(
            "[HDR] wgpu pipeline cache unchanged, skip write {}",
            path.display()
        );
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = temp_path_for(path);
    std::fs::write(&temp, data)?;
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    remember_saved(path, data);
    log::info!(
        "[HDR] saved wgpu pipeline cache {} ({} bytes)",
        path.display(),
        data.len()
    );
    Ok(())
}

pub fn create_pipeline_cache(
    device: &wgpu::Device,
    adapter: &wgpu::Adapter,
) -> Option<wgpu::PipelineCache> {
    if !adapter_supports_pipeline_cache(adapter) {
        // No live cache this session. Still drop files from older schema versions.
        start_pipeline_cache_cleanup(None);
        log::info!(
            "[startup] wgpu PipelineCache unsupported on adapter \"{}\" ({:?}); \
             continuing without on-disk pipeline cache",
            adapter.get_info().name,
            adapter.get_info().backend,
        );
        return None;
    }
    if !device.features().contains(wgpu::Features::PIPELINE_CACHE) {
        start_pipeline_cache_cleanup(None);
        log::warn!(
            "[startup] adapter advertised PipelineCache but device lacks the feature; \
             continuing without on-disk pipeline cache"
        );
        return None;
    }
    let path = cache_path(adapter);
    start_pipeline_cache_cleanup(Some(path.clone()));
    let cache_data = load_for_adapter(adapter);
    debug_assert!(
        path.to_string_lossy()
            .contains(&format!("_pcv{PIPELINE_CACHE_SCHEMA_VERSION}_")),
        "pipeline cache path must embed schema version"
    );
    // SAFETY: `cache_data` comes from our own prior `PipelineCache::get_data` writes.
    Some(unsafe {
        device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
            label: Some("simple-image-viewer-pipeline-cache"),
            data: cache_data.as_deref(),
            fallback: true,
        })
    })
}

/// Windows adapter convenience wrapper for [`persist`]. Requires a live cache instance.
#[cfg(target_os = "windows")]
#[allow(dead_code)] // reserved for non-DX12 adapters; DX12 uses PipelineCache auto-persist
pub fn persist_adapter(adapter: &wgpu::Adapter, cache: &wgpu::PipelineCache) {
    persist(&adapter.get_info(), cache);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter_info(driver: &str) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: "NVIDIA GeForce RTX 4070".to_owned(),
            vendor: 0x10de,
            device: 0x2786,
            device_type: wgpu::DeviceType::DiscreteGpu,
            device_pci_bus_id: String::new(),
            driver: driver.to_owned(),
            driver_info: String::new(),
            backend: wgpu::Backend::Dx12,
            subgroup_min_size: 32,
            subgroup_max_size: 32,
            transient_saves_memory: false,
        }
    }

    #[test]
    fn cache_path_changes_with_driver_version() {
        let old_driver = cache_path_for_adapter_info(&adapter_info("32.0.16.1052"));
        let new_driver = cache_path_for_adapter_info(&adapter_info("32.0.16.2000"));

        assert_ne!(old_driver.file_name(), new_driver.file_name());
    }

    #[test]
    fn cache_path_includes_schema_version() {
        let path = cache_path_for_adapter_info(&adapter_info("32.0.16.1052"));
        let file_name = path.file_name().unwrap().to_string_lossy();

        assert!(file_name.contains(&format!("_pcv{PIPELINE_CACHE_SCHEMA_VERSION}_")));
    }

    fn test_dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "siv-pcache-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_to_path_renames_temp_file_away() {
        let dir = test_dir("save");
        let path = dir.join("siv_wgpu_pipeline_cache_Dx12_4318_10464_pcv11_drv1.bin");
        let temp = temp_path_for(&path);

        save_to_path(&path, b"cache-bytes").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"cache-bytes");
        assert!(!temp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_to_path_skips_write_when_bytes_are_unchanged() {
        let dir = test_dir("unchanged");
        let path = dir.join("siv_wgpu_pipeline_cache_Dx12_4318_10464_pcv11_drv1.bin");

        save_to_path(&path, b"cache-bytes").unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        save_to_path(&path, b"cache-bytes").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"tampered",
            "identical bytes must not rewrite the file"
        );

        save_to_path(&path, b"cache-bytes-v2").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"cache-bytes-v2");
        assert!(!temp_path_for(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocking_persist_joins_in_flight_save_before_returning() {
        static ORDER: Mutex<()> = Mutex::new(());
        let _order = ORDER.lock();

        let dir = test_dir("join");
        let path = dir.join("siv_wgpu_pipeline_cache_Dx12_4318_10464_pcv11_drv1.bin");
        spawn_persist(path.clone(), b"from-background".to_vec());
        finish_persist_thread();

        assert_eq!(std::fs::read(&path).unwrap(), b"from-background");
        assert!(!temp_path_for(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_discards_zero_byte_temp_left_by_a_killed_writer() {
        let dir = test_dir("discard");
        let path = dir.join("siv_wgpu_pipeline_cache_Dx12_4318_10464_pcv11_drv1.bin");
        let temp = temp_path_for(&path);
        std::fs::write(&path, b"good-cache").unwrap();
        std::fs::write(&temp, b"").unwrap();

        discard_incomplete_temp(&path);

        assert_eq!(std::fs::read(&path).unwrap(), b"good-cache");
        assert!(!temp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn versioned(prefix: &str, version: u32, driver: &str, ext: &str) -> String {
        format!("{prefix}{PIPELINE_CACHE_VERSION_MARK}{version}_drv{driver}.{ext}")
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn cleanup_keeps_live_cache_and_removes_old_versions() {
        let dir = test_dir("cleanup");
        // Device id 10464 is a string prefix of 104640. The longer id must survive.
        let live_prefix = "wgpu_pipeline_cache_vulkan_4318_10464";
        let live = versioned(
            live_prefix,
            PIPELINE_CACHE_SCHEMA_VERSION,
            "aaaaaaaaaaaaaaaa",
            "bin",
        );
        let live_tmp = versioned(
            live_prefix,
            PIPELINE_CACHE_SCHEMA_VERSION,
            "aaaaaaaaaaaaaaaa",
            "tmp",
        );
        let old_bin = versioned(
            live_prefix,
            PIPELINE_CACHE_SCHEMA_VERSION - 1,
            "aaaaaaaaaaaaaaaa",
            "bin",
        );
        let old_tmp = versioned(
            live_prefix,
            PIPELINE_CACHE_SCHEMA_VERSION - 1,
            "bbbbbbbbbbbbbbbb",
            "tmp",
        );
        let other_driver = versioned(
            live_prefix,
            PIPELINE_CACHE_SCHEMA_VERSION,
            "bbbbbbbbbbbbbbbb",
            "bin",
        );
        let newer = versioned(
            live_prefix,
            PIPELINE_CACHE_SCHEMA_VERSION + 1,
            "cccccccccccccccc",
            "bin",
        );
        let other_gpu = versioned(
            "wgpu_pipeline_cache_vulkan_4318_104640",
            PIPELINE_CACHE_SCHEMA_VERSION,
            "dddddddddddddddd",
            "bin",
        );
        let other_backend_current = versioned(
            "siv_wgpu_pipeline_cache_Dx12_4318_10464",
            PIPELINE_CACHE_SCHEMA_VERSION,
            "ffffffffeeeeeeee",
            "bin",
        );
        let other_gpu_old = versioned(
            "wgpu_pipeline_cache_vulkan_99_100",
            PIPELINE_CACHE_SCHEMA_VERSION - 1,
            "eeeeeeeeeeeeeeee",
            "bin",
        );
        let unversioned = "siv_wgpu_pipeline_cache_Dx12_1_2.bin";
        let kept = [
            live.as_str(),
            live_tmp.as_str(),
            newer.as_str(),
            other_gpu.as_str(),
            other_backend_current.as_str(),
            "siv_settings.yaml",
            "siv_wgpu_preprobe_cache.yaml",
            "notes.bin",
            "图片.png",
        ];
        let removed = [
            old_bin.as_str(),
            old_tmp.as_str(),
            other_driver.as_str(),
            other_gpu_old.as_str(),
            unversioned,
        ];
        for name in kept.iter().chain(removed.iter()) {
            touch(&dir, name);
        }
        let decoy_dir = dir.join(versioned(
            "siv_wgpu_pipeline_cache_not_a_file",
            PIPELINE_CACHE_SCHEMA_VERSION - 1,
            "1",
            "bin",
        ));
        std::fs::create_dir(&decoy_dir).unwrap();

        let live_path = dir.join(&live);
        remove_old_pipeline_cache_files(&dir, Some(&live_path));

        for name in kept {
            assert!(dir.join(name).is_file(), "kept {name}");
        }
        for name in removed {
            assert!(!dir.join(name).exists(), "removed {name}");
        }
        assert!(decoy_dir.is_dir(), "directory entries are not deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_without_live_cache_keeps_current_schema_files() {
        let dir = test_dir("cleanup-none");
        let current = versioned(
            "siv_wgpu_pipeline_cache_Metal_1_2",
            PIPELINE_CACHE_SCHEMA_VERSION,
            "1",
            "bin",
        );
        let old = versioned(
            "siv_wgpu_pipeline_cache_Metal_1_2",
            PIPELINE_CACHE_SCHEMA_VERSION - 1,
            "1",
            "bin",
        );
        touch(&dir, &current);
        touch(&dir, &old);

        remove_old_pipeline_cache_files(&dir, None);

        assert!(dir.join(&current).is_file());
        assert!(!dir.join(&old).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn protected_name_match_ignores_ascii_case() {
        let live = versioned(
            "siv_wgpu_pipeline_cache_Dx12_1_2",
            PIPELINE_CACHE_SCHEMA_VERSION,
            "1",
            "bin",
        );
        let protected = protected_file_names(Path::new(&live));
        let prefix = adapter_cache_prefix(&live);
        let upper = OsString::from(live.to_ascii_uppercase());
        assert!(!should_remove_pipeline_cache_file(
            &upper, &protected, prefix
        ));
    }

    #[test]
    fn cleanup_thread_deletes_old_cache_and_joins() {
        static ORDER: Mutex<()> = Mutex::new(());
        let _order = ORDER.lock();

        // Nothing started yet: join must return without waiting.
        join_pipeline_cache_cleanup();

        let dir = test_dir("cleanup-join");
        let live = dir.join(versioned(
            "siv_wgpu_pipeline_cache_Dx12_7_8",
            PIPELINE_CACHE_SCHEMA_VERSION,
            "1",
            "bin",
        ));
        let old = dir.join(versioned(
            "siv_wgpu_pipeline_cache_Dx12_7_8",
            PIPELINE_CACHE_SCHEMA_VERSION - 1,
            "1",
            "bin",
        ));
        std::fs::write(&live, b"live").unwrap();
        std::fs::write(&old, b"old").unwrap();

        start_pipeline_cache_cleanup(Some(live.clone()));
        // A second start must not spawn another thread (the first scan is the only one).
        start_pipeline_cache_cleanup(None);
        join_pipeline_cache_cleanup();
        join_pipeline_cache_cleanup();

        assert_eq!(std::fs::read(&live).unwrap(), b"live");
        assert!(!old.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
