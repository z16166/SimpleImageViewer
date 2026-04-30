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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrPresentationPath {
    WindowsDx12ScRgb,
    MacOsMetalEdr,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct HdrCapabilities {
    pub available: bool,
    pub output_mode: HdrOutputMode,
    pub backend: Option<wgpu::Backend>,
    pub current_surface_format: Option<wgpu::TextureFormat>,
    pub candidate_platform_path: Option<HdrPresentationPath>,
    pub native_presentation_enabled: bool,
    pub reason: String,
    pub candidate_texture_format: Option<wgpu::TextureFormat>,
}

impl HdrCapabilities {
    #[allow(dead_code)]
    pub fn sdr(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            backend: None,
            current_surface_format: None,
            candidate_platform_path: None,
            native_presentation_enabled: false,
            reason: reason.into(),
            candidate_texture_format: None,
        }
    }

    pub fn startup_diagnostics(&self) -> Vec<String> {
        vec![
            format!("[HDR] backend={:?}", self.backend),
            format!(
                "[HDR] current_surface_format={:?}",
                self.current_surface_format
            ),
            format!("[HDR] mode={:?}", self.output_mode),
            format!("[HDR] available={}", self.available),
            format!(
                "[HDR] native_presentation_enabled={}",
                self.native_presentation_enabled
            ),
            format!("[HDR] reason={}", self.reason),
            format!(
                "[HDR] candidate_platform_path={:?}",
                self.candidate_platform_path
            ),
            format!(
                "[HDR] candidate_texture_format={:?}",
                self.candidate_texture_format
            ),
        ]
    }

    fn candidate(
        backend: wgpu::Backend,
        current_surface_format: Option<wgpu::TextureFormat>,
        candidate_platform_path: HdrPresentationPath,
        native_output_mode: HdrOutputMode,
        reason: impl Into<String>,
    ) -> Self {
        let native_presentation_enabled =
            crate::hdr::surface::is_native_hdr_surface_format(current_surface_format);
        Self {
            available: native_presentation_enabled,
            output_mode: if native_presentation_enabled {
                native_output_mode
            } else {
                HdrOutputMode::SdrToneMapped
            },
            backend: Some(backend),
            current_surface_format,
            candidate_platform_path: Some(candidate_platform_path),
            native_presentation_enabled,
            reason: if native_presentation_enabled {
                reason.into()
            } else {
                let blocker =
                    crate::hdr::surface::native_hdr_surface_blocker(current_surface_format)
                        .unwrap_or("native HDR presentation is not active");
                format!("{}; {blocker}", reason.into())
            },
            candidate_texture_format: Some(wgpu::TextureFormat::Rgba16Float),
        }
    }

    fn unsupported_backend(
        backend: wgpu::Backend,
        current_surface_format: Option<wgpu::TextureFormat>,
    ) -> Self {
        Self {
            backend: Some(backend),
            current_surface_format,
            reason: format!("unsupported HDR backend: {backend:?}"),
            ..Self::sdr("")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdr_capabilities_expose_hdr_presentation_state() {
        let capabilities = HdrCapabilities::sdr("wgpu render state unavailable");
        let formatted = format!("{capabilities:?}");

        assert!(!capabilities.available);
        assert_eq!(capabilities.output_mode, HdrOutputMode::SdrToneMapped);
        assert_eq!(capabilities.backend, None);
        assert_eq!(capabilities.candidate_platform_path, None);
        assert!(!capabilities.native_presentation_enabled);
        assert_eq!(capabilities.candidate_texture_format, None);
        assert!(formatted.contains("native_presentation_enabled"));
        assert!(formatted.contains("candidate_texture_format"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn dx12_is_reported_as_candidate_not_enabled() {
        let capabilities = detect_from_backend(wgpu::Backend::Dx12);

        assert!(!capabilities.available);
        assert_eq!(capabilities.output_mode, HdrOutputMode::SdrToneMapped);
        assert_eq!(capabilities.backend, Some(wgpu::Backend::Dx12));
        assert_eq!(
            capabilities.current_surface_format,
            Some(wgpu::TextureFormat::Bgra8Unorm)
        );
        assert_eq!(
            capabilities.candidate_platform_path,
            Some(HdrPresentationPath::WindowsDx12ScRgb)
        );
        assert!(!capabilities.native_presentation_enabled);
        assert_eq!(
            capabilities.candidate_texture_format,
            Some(wgpu::TextureFormat::Rgba16Float)
        );
        assert!(
            capabilities
                .reason
                .contains("DX12 backend detected, scRGB/HDR swapchain configuration")
        );
        assert!(
            capabilities
                .reason
                .contains("current eframe/wgpu target format is SDR")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn dx12_with_float_surface_enables_windows_scrgb_native_presentation() {
        let capabilities = detect_from_backend_and_surface_format(
            wgpu::Backend::Dx12,
            Some(wgpu::TextureFormat::Rgba16Float),
        );

        assert!(capabilities.available);
        assert_eq!(capabilities.output_mode, HdrOutputMode::WindowsScRgb);
        assert_eq!(capabilities.backend, Some(wgpu::Backend::Dx12));
        assert_eq!(
            capabilities.current_surface_format,
            Some(wgpu::TextureFormat::Rgba16Float)
        );
        assert!(capabilities.native_presentation_enabled);
        assert!(
            capabilities
                .reason
                .contains("DX12 backend with float surface format")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn metal_is_reported_as_candidate_not_enabled() {
        let capabilities = detect_from_backend(wgpu::Backend::Metal);

        assert!(!capabilities.available);
        assert_eq!(capabilities.output_mode, HdrOutputMode::SdrToneMapped);
        assert_eq!(capabilities.backend, Some(wgpu::Backend::Metal));
        assert_eq!(
            capabilities.candidate_platform_path,
            Some(HdrPresentationPath::MacOsMetalEdr)
        );
        assert!(!capabilities.native_presentation_enabled);
        assert_eq!(
            capabilities.candidate_texture_format,
            Some(wgpu::TextureFormat::Rgba16Float)
        );
        assert!(
            capabilities
                .reason
                .contains("Metal backend detected, EDR CAMetalLayer configuration")
        );
        assert!(
            capabilities
                .reason
                .contains("current eframe/wgpu target format is SDR")
        );
    }

    #[test]
    fn unsupported_backend_stays_sdr_without_candidate_path() {
        let capabilities = detect_from_backend(wgpu::Backend::Vulkan);

        assert!(!capabilities.available);
        assert_eq!(capabilities.output_mode, HdrOutputMode::SdrToneMapped);
        assert_eq!(capabilities.backend, Some(wgpu::Backend::Vulkan));
        assert_eq!(
            capabilities.current_surface_format,
            Some(wgpu::TextureFormat::Bgra8Unorm)
        );
        assert_eq!(capabilities.candidate_platform_path, None);
        assert!(!capabilities.native_presentation_enabled);
        assert_eq!(capabilities.candidate_texture_format, None);
        assert_eq!(capabilities.reason, "unsupported HDR backend: Vulkan");
    }

    #[test]
    fn startup_diagnostics_include_required_fields() {
        let capabilities = HdrCapabilities::sdr("wgpu render state unavailable");
        let diagnostics = capabilities.startup_diagnostics();

        assert_eq!(
            diagnostics,
            [
                "[HDR] backend=None",
                "[HDR] current_surface_format=None",
                "[HDR] mode=SdrToneMapped",
                "[HDR] available=false",
                "[HDR] native_presentation_enabled=false",
                "[HDR] reason=wgpu render state unavailable",
                "[HDR] candidate_platform_path=None",
                "[HDR] candidate_texture_format=None",
            ]
        );
    }
}

#[allow(dead_code)]
pub fn detect_from_wgpu_state(state: Option<&eframe::egui_wgpu::RenderState>) -> HdrCapabilities {
    let Some(state) = state else {
        return HdrCapabilities::sdr("wgpu render state unavailable");
    };

    detect_from_backend_and_surface_format(
        state.adapter.get_info().backend,
        Some(state.target_format),
    )
}

#[cfg(test)]
fn detect_from_backend(backend: wgpu::Backend) -> HdrCapabilities {
    detect_from_backend_and_surface_format(backend, Some(wgpu::TextureFormat::Bgra8Unorm))
}

pub fn detect_from_backend_and_surface_format(
    backend: wgpu::Backend,
    current_surface_format: Option<wgpu::TextureFormat>,
) -> HdrCapabilities {
    match backend {
        #[cfg(target_os = "windows")]
        wgpu::Backend::Dx12 => HdrCapabilities::candidate(
            backend,
            current_surface_format,
            HdrPresentationPath::WindowsDx12ScRgb,
            HdrOutputMode::WindowsScRgb,
            if crate::hdr::surface::is_native_hdr_surface_format(current_surface_format) {
                "DX12 backend with float surface format; Windows scRGB native presentation path is active"
            } else {
                "DX12 backend detected, scRGB/HDR swapchain configuration candidate"
            },
        ),
        #[cfg(target_os = "macos")]
        wgpu::Backend::Metal => HdrCapabilities::candidate(
            backend,
            current_surface_format,
            HdrPresentationPath::MacOsMetalEdr,
            HdrOutputMode::MacOsEdr,
            if crate::hdr::surface::is_native_hdr_surface_format(current_surface_format) {
                "Metal backend with float surface format; macOS EDR native presentation path is active"
            } else {
                "Metal backend detected, EDR CAMetalLayer configuration candidate"
            },
        ),
        _ => HdrCapabilities::unsupported_backend(backend, current_surface_format),
    }
}
