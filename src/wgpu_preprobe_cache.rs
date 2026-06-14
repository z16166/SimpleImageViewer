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

//! Persisted result of the Windows wgpu DX12 adapter pre-probe (`siv_wgpu_preprobe_cache.yaml`).
//! Startup applies this optimistically when present; a background `enumerate_adapters` may still
//! run without blocking the main thread. If the live result disagrees with yaml, the background
//! thread rewrites this file for the **next** launch (the current session keeps cache-backed
//! [`eframe::egui_wgpu::WgpuSetup`]). Delete or edit `force_dx12` if the UI fails to create.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WgpuPreprobeCache {
    /// Bump when the on-disk schema changes; unknown versions are ignored and re-probed.
    pub format_version: u32,
    /// When `true`, matches fresh detection with a discrete/integrated DX12 adapter:
    /// `Backends::DX12` + `PowerPreference::HighPerformance`.
    pub force_dx12: bool,
}

impl WgpuPreprobeCache {
    pub fn new(force_dx12: bool) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            force_dx12,
        }
    }
}

pub fn cache_path() -> PathBuf {
    crate::settings::settings_path().with_file_name("siv_wgpu_preprobe_cache.yaml")
}

pub fn load() -> Option<WgpuPreprobeCache> {
    let path = cache_path();
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_yaml::from_str::<WgpuPreprobeCache>(&text) {
        Ok(c) => Some(c),
        Err(e) => {
            log::warn!(
                "Ignoring invalid wgpu preprobe cache {}: {e}",
                path.display(),
            );
            None
        }
    }
}

pub fn save(force_dx12: bool) -> std::io::Result<()> {
    let path = cache_path();
    let payload = WgpuPreprobeCache::new(force_dx12);
    let yaml = serde_yaml::to_string(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(&path, yaml)
}
