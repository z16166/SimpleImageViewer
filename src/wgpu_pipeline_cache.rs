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

pub fn cache_path_for_adapter_info(info: &wgpu::AdapterInfo) -> PathBuf {
    let stem = wgpu::util::pipeline_cache_key(info).unwrap_or_else(|| {
        format!(
            "siv_wgpu_pipeline_cache_{:?}_{}_{}",
            info.backend, info.vendor, info.device
        )
    });
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

pub fn load_for_adapter(adapter: &wgpu::Adapter) -> Option<Vec<u8>> {
    let path = cache_path(adapter);
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

pub fn persist(info: &wgpu::AdapterInfo, cache: &wgpu::PipelineCache) {
    let Some(data) = cache.get_data() else {
        return;
    };
    if let Err(error) = save_to_path(&cache_path_for_adapter_info(info), &data) {
        log::warn!("[HDR] failed to save wgpu pipeline cache: {error}");
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
    std::fs::rename(temp, &path)?;
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
) -> wgpu::PipelineCache {
    let cache_data = load_for_adapter(adapter);
    // SAFETY: `cache_data` comes from our own prior `PipelineCache::get_data` writes.
    unsafe {
        device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
            label: Some("simple-image-viewer-pipeline-cache"),
            data: cache_data.as_deref(),
            fallback: true,
        })
    }
}

#[cfg(target_os = "windows")]
#[allow(dead_code)] // reserved for non-DX12 adapters; DX12 uses PipelineCache auto-persist
pub fn persist_adapter(adapter: &wgpu::Adapter, cache: &wgpu::PipelineCache) {
    persist(&adapter.get_info(), cache);
}
