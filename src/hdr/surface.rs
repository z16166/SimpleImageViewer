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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrSurfaceSelection {
    NativeHdr(wgpu::TextureFormat),
    Unavailable { reason: &'static str },
}

pub fn choose_native_hdr_surface_format(formats: &[wgpu::TextureFormat]) -> HdrSurfaceSelection {
    for preferred in [
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureFormat::Rgba32Float,
    ] {
        if formats.contains(&preferred) {
            return HdrSurfaceSelection::NativeHdr(preferred);
        }
    }

    HdrSurfaceSelection::Unavailable {
        reason: "surface exposes no float HDR presentation format",
    }
}

pub fn preferred_native_hdr_target_format_for_platform() -> Option<wgpu::TextureFormat> {
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        Some(wgpu::TextureFormat::Rgba16Float)
    } else {
        None
    }
}

pub fn preferred_native_hdr_target_format_for_settings(
    native_surface_enabled: bool,
) -> Option<wgpu::TextureFormat> {
    if native_surface_enabled {
        preferred_native_hdr_target_format_for_platform()
    } else {
        None
    }
}

pub fn is_native_hdr_surface_format(format: Option<wgpu::TextureFormat>) -> bool {
    let Some(format) = format else {
        return false;
    };

    matches!(
        choose_native_hdr_surface_format(&[format]),
        HdrSurfaceSelection::NativeHdr(_)
    )
}

pub fn native_hdr_surface_blocker(format: Option<wgpu::TextureFormat>) -> Option<&'static str> {
    if is_native_hdr_surface_format(format) {
        return None;
    }

    Some(match format {
        Some(_) => "current eframe/wgpu target format is SDR; native HDR requires a float surface",
        None => "current eframe/wgpu target format is unknown; native HDR requires a float surface",
    })
}

#[cfg(test)]
mod tests {
    use crate::hdr::surface::{
        HdrSurfaceSelection, choose_native_hdr_surface_format, native_hdr_surface_blocker,
    };

    #[test]
    fn prefers_rgba16_float_for_native_hdr_surface() {
        let selection = choose_native_hdr_surface_format(&[
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Rgba8Unorm,
        ]);

        assert_eq!(
            selection,
            HdrSurfaceSelection::NativeHdr(wgpu::TextureFormat::Rgba16Float)
        );
    }

    #[test]
    fn reports_blocker_when_only_sdr_formats_are_available() {
        let selection = choose_native_hdr_surface_format(&[
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ]);

        assert_eq!(
            selection,
            HdrSurfaceSelection::Unavailable {
                reason: "surface exposes no float HDR presentation format"
            }
        );
    }

    #[test]
    fn reports_current_sdr_target_format_as_native_hdr_blocker() {
        assert_eq!(
            native_hdr_surface_blocker(Some(wgpu::TextureFormat::Bgra8Unorm)),
            Some("current eframe/wgpu target format is SDR; native HDR requires a float surface")
        );
        assert_eq!(
            native_hdr_surface_blocker(Some(wgpu::TextureFormat::Rgba16Float)),
            None
        );
    }

    #[test]
    fn reports_unknown_target_format_separately_from_sdr() {
        assert_eq!(
            native_hdr_surface_blocker(None),
            Some(
                "current eframe/wgpu target format is unknown; native HDR requires a float surface"
            )
        );
    }

    #[test]
    fn platform_native_hdr_request_is_limited_to_windows_and_macos() {
        let expected = if cfg!(any(target_os = "windows", target_os = "macos")) {
            Some(wgpu::TextureFormat::Rgba16Float)
        } else {
            None
        };

        assert_eq!(
            super::preferred_native_hdr_target_format_for_platform(),
            expected
        );
    }

    #[test]
    fn disabled_native_hdr_request_returns_no_preferred_target_format() {
        assert_eq!(
            super::preferred_native_hdr_target_format_for_settings(false),
            None
        );
    }
}
