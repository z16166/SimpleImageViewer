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

use eframe::egui_wgpu;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrRenderOutputMode {
    SdrToneMapped = 0,
    /// Linear scRGB / EDR (`Rgba16Float`, `Rgba32Float`).
    NativeHdr = 1,
    /// PQ HDR10 (`Rgb10a2Unorm` + compositor ST 2084).
    NativeHdrPq = 2,
    /// Gamma 2.2 electrical for KWin KMS HDR offload (`Rgb10a2Unorm`).
    NativeHdrGamma22 = 3,
}

impl HdrRenderOutputMode {
    pub fn for_target_format(
        target_format: wgpu::TextureFormat,
        native_surface_encoding: Option<crate::hdr::monitor::HdrNativeSurfaceEncoding>,
    ) -> Self {
        use crate::hdr::monitor::HdrNativeSurfaceEncoding;
        match target_format {
            wgpu::TextureFormat::Rgb10a2Unorm => match native_surface_encoding {
                Some(HdrNativeSurfaceEncoding::PqHdr10) => Self::NativeHdrPq,
                Some(HdrNativeSurfaceEncoding::Gamma22Electrical) => Self::NativeHdrGamma22,
                Some(HdrNativeSurfaceEncoding::LinearScRgb) => Self::NativeHdrGamma22,
                None => Self::SdrToneMapped,
            },
            wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float => Self::NativeHdr,
            format if crate::hdr::surface::is_native_hdr_surface_format(Some(format)) => {
                Self::NativeHdr
            }
            _ => Self::SdrToneMapped,
        }
    }

    pub fn is_native_hdr(self) -> bool {
        matches!(
            self,
            Self::NativeHdr | Self::NativeHdrPq | Self::NativeHdrGamma22
        )
    }

    pub fn as_diagnostic_label(self) -> &'static str {
        match self {
            Self::NativeHdr => "native_hdr",
            Self::NativeHdrPq => "native_hdr_pq",
            Self::NativeHdrGamma22 => "native_hdr_gamma22",
            Self::SdrToneMapped => "sdr_tone_mapped",
        }
    }

    pub fn rgb10a2_uses_pq_shader(self) -> bool {
        matches!(self, Self::NativeHdrPq)
    }
}

/// When [`HdrRenderOutputMode::SdrToneMapped`] composites into **`Rgba8Unorm` / `Bgra8Unorm`**, the GPU stores
/// fragment output **literally** in 8‑bit channels (`encode_sdr` must apply IEC 61966‑2‑1 / ~gamma OETF in WGSL).
///
/// **`Rgba8UnormSrgb` / `Bgra8UnormSrgb`** treat fragment output as **linear display RGB** and **apply sRGB encode on write**
/// ([`wgpu` texture conventions](https://github.com/gfx-rs/wgpu/wiki/Texture-Color-Formats-and-Srgb-conversions)). Emitting pre‑encoded
/// values from WGSL (**double‑OETF**) lifts mids / washes contrast (**「灰蒙蒙」** vs Chrome on SDR canvases).
pub(crate) fn hdr_sdr_framebuffer_needs_manual_srgb_oetf(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
    )
}

pub fn hdr_render_output_diagnostics(target_format: Option<wgpu::TextureFormat>) -> [String; 2] {
    let output_mode =
        target_format.map(|format| HdrRenderOutputMode::for_target_format(format, None));
    [
        format!("[HDR] render_target_format={target_format:?}"),
        format!(
            "[HDR] shader_output_mode={}",
            output_mode
                .map(HdrRenderOutputMode::as_diagnostic_label)
                .unwrap_or("unknown")
        ),
    ]
}

pub fn hdr_egui_overlay_diagnostics(target_format: Option<wgpu::TextureFormat>) -> [String; 2] {
    let shader_entry_point = target_format.map(|format| {
        let rgb10a2_pq = matches!(format, wgpu::TextureFormat::Rgb10a2Unorm)
            && HdrRenderOutputMode::for_target_format(format, None)
                == HdrRenderOutputMode::NativeHdrPq;
        egui_wgpu::egui_framebuffer_shader_entry_point(format, rgb10a2_pq)
    });
    [
        format!("[HDR] egui_overlay_target_format={target_format:?}"),
        format!(
            "[HDR] egui_overlay_framebuffer_shader={}",
            shader_entry_point.unwrap_or("unknown")
        ),
    ]
}
