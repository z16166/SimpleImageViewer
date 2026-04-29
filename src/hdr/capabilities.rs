use super::types::HdrOutputMode;

#[derive(Debug, Clone)]
pub struct HdrCapabilities {
    pub available: bool,
    pub output_mode: HdrOutputMode,
    pub reason: String,
    pub preferred_texture_format: Option<wgpu::TextureFormat>,
}

impl HdrCapabilities {
    pub fn sdr(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            reason: reason.into(),
            preferred_texture_format: None,
        }
    }
}

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
            preferred_texture_format: Some(wgpu::TextureFormat::Rgba16Float),
        },
        #[cfg(target_os = "macos")]
        wgpu::Backend::Metal => HdrCapabilities {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            reason: "Metal available; EDR layer configuration pending".to_string(),
            preferred_texture_format: Some(wgpu::TextureFormat::Rgba16Float),
        },
        _ => HdrCapabilities::sdr(format!("unsupported HDR backend: {backend:?}")),
    }
}
