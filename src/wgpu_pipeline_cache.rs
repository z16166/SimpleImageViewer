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

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::Mutex;

/// Bump when wgpu render pipelines or WGSL change in ways that invalidate on-disk cache bytes.
const PIPELINE_CACHE_SCHEMA_VERSION: u32 = 8;

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
    remove_stale_pipeline_cache_files(&path);
    match std::fs::read(&path) {
        Ok(data) => {
            log::info!(
                "[HDR] loaded wgpu pipeline cache {} ({} bytes)",
                path.display(),
                data.len()
            );
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
    let mut in_flight = persist_in_flight().lock();
    if in_flight.as_ref() == Some(&path) {
        return;
    }
    *in_flight = Some(path.clone());
    drop(in_flight);

    if let Err(err) = std::thread::Builder::new()
        .name("siv-wgpu-pcache-persist".to_string())
        .spawn(move || {
            let result = save_to_path(&path, &data);
            persist_in_flight().lock().take();
            if let Err(error) = result {
                log::warn!("[HDR] failed to save wgpu pipeline cache: {error}");
            }
        })
    {
        persist_in_flight().lock().take();
        log::warn!("[HDR] failed to spawn wgpu pipeline cache persist thread: {err}");
    }
}

fn persist_in_flight() -> &'static Mutex<Option<PathBuf>> {
    static GATE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(None))
}

fn remove_stale_pipeline_cache_files(current_path: &Path) {
    let Some(parent) = current_path.parent() else {
        return;
    };
    let current_name = current_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let Some(current_name) = current_name else {
        return;
    };
    let Some(base_prefix) = current_name.split("_pcv").next() else {
        return;
    };
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == current_name {
            continue;
        }
        if name.starts_with(base_prefix) && name.ends_with(".bin") && name.contains("_pcv") {
            if let Err(error) = std::fs::remove_file(entry.path()) {
                log::debug!(
                    "[HDR] failed to remove stale wgpu pipeline cache {}: {error}",
                    entry.path().display()
                );
            } else {
                log::info!(
                    "[HDR] removed stale wgpu pipeline cache {}",
                    entry.path().display()
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

fn save_to_path(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, data)?;
    std::fs::rename(temp, path)?;
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
        log::info!(
            "[startup] wgpu PipelineCache unsupported on adapter \"{}\" ({:?}); \
             continuing without on-disk pipeline cache",
            adapter.get_info().name,
            adapter.get_info().backend,
        );
        return None;
    }
    if !device.features().contains(wgpu::Features::PIPELINE_CACHE) {
        log::warn!(
            "[startup] adapter advertised PipelineCache but device lacks the feature; \
             continuing without on-disk pipeline cache"
        );
        return None;
    }
    let cache_data = load_for_adapter(adapter);
    let path = cache_path(adapter);
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

        assert!(file_name.contains("_pcv5_"));
    }
}
