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

use super::capabilities::{HdrCapabilities, HdrPresentationPath};
use super::types::HdrOutputMode;
use rust_i18n::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrRenderPath {
    FloatImagePlane,
    SdrFallback,
}

pub fn hdr_osd_tag(
    is_hdr_source: bool,
    render_path: HdrRenderPath,
    capabilities: &HdrCapabilities,
) -> Option<String> {
    if !is_hdr_source {
        return None;
    }

    let render = hdr_render_path_label(render_path);
    let output = hdr_output_label(capabilities);

    Some(format!("HDR: source | {render} | {output}"))
}

fn hdr_render_path_label(render_path: HdrRenderPath) -> String {
    match render_path {
        HdrRenderPath::FloatImagePlane => t!("hdr.render_path.float_plane").to_string(),
        HdrRenderPath::SdrFallback => t!("hdr.render_path.sdr_fallback").to_string(),
    }
}

pub fn hdr_output_label(capabilities: &HdrCapabilities) -> String {
    if capabilities.native_presentation_enabled {
        match capabilities.output_mode {
            HdrOutputMode::WindowsScRgb => t!("hdr.output.windows_scrgb").to_string(),
            HdrOutputMode::MacOsEdr => t!("hdr.output.macos_edr").to_string(),
            HdrOutputMode::SdrToneMapped => t!("hdr.output.native_hdr").to_string(),
        }
    } else {
        t!("hdr.output.sdr_tone_mapped").to_string()
    }
}

pub fn hdr_candidate_label(capabilities: &HdrCapabilities) -> String {
    match capabilities.candidate_platform_path {
        Some(HdrPresentationPath::WindowsDx12ScRgb) => {
            t!("hdr.candidate.windows_dx12_scrgb").to_string()
        }
        Some(HdrPresentationPath::MacOsMetalEdr) => t!("hdr.candidate.macos_metal_edr").to_string(),
        None => t!("hdr.candidate.none").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::hdr::capabilities::HdrCapabilities;
    use crate::hdr::status::{HdrRenderPath, hdr_osd_tag};

    #[test]
    fn hdr_osd_tag_names_float_plane_and_sdr_output() {
        let tag = hdr_osd_tag(
            true,
            HdrRenderPath::FloatImagePlane,
            &HdrCapabilities::sdr("native HDR output not enabled"),
        );

        assert_eq!(
            tag.as_deref(),
            Some("HDR: source | plane | SDR tone-mapped")
        );
    }

    #[test]
    fn hdr_osd_tag_is_hidden_for_non_hdr_images() {
        let tag = hdr_osd_tag(
            false,
            HdrRenderPath::SdrFallback,
            &HdrCapabilities::sdr("not an HDR image"),
        );

        assert_eq!(tag, None);
    }
}
