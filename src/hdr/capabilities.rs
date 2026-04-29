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

use super::types::HdrOutputMode;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct HdrCapabilities {
    pub available: bool,
    pub output_mode: HdrOutputMode,
    pub reason: String,
    pub preferred_texture_format: Option<wgpu::TextureFormat>,
}

impl HdrCapabilities {
    #[allow(dead_code)]
    pub fn sdr(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            reason: reason.into(),
            preferred_texture_format: None,
        }
    }
}

#[allow(dead_code)]
pub fn detect_from_wgpu_state(state: Option<&eframe::egui_wgpu::RenderState>) -> HdrCapabilities {
    let Some(state) = state else {
        return HdrCapabilities::sdr("wgpu render state unavailable");
    };

    let backend = state.adapter.get_info().backend;
    match backend {
        #[cfg(target_os = "windows")]
        wgpu::Backend::Dx12 => HdrCapabilities {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            reason: "DX12 available; HDR surface configuration pending".to_string(),
            preferred_texture_format: None,
        },
        #[cfg(target_os = "macos")]
        wgpu::Backend::Metal => HdrCapabilities {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            reason: "Metal available; EDR layer configuration pending".to_string(),
            preferred_texture_format: None,
        },
        _ => HdrCapabilities::sdr(format!("unsupported HDR backend: {backend:?}")),
    }
}
