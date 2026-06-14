// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// SPDX-License-Identifier: GPL-3.0-only

//! On-disk persistence for [`wgpu::PipelineCache`] (DX12 cached PSO blobs via patched wgpu-hal).

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

pub fn persist_adapter(adapter: &wgpu::Adapter, cache: &wgpu::PipelineCache) {
    persist(&adapter.get_info(), cache);
}
