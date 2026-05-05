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

use super::capabilities::HdrCapabilities;
use super::types::{HdrColorSpace, HdrOutputMode};
use rust_i18n::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrRenderPath {
    FloatImagePlane,
    FloatTilePlane,
    SdrFallback,
}

pub fn hdr_osd_tag(
    is_hdr_source: bool,
    render_path: HdrRenderPath,
    color_space: Option<HdrColorSpace>,
    capabilities: &HdrCapabilities,
    ultra_hdr_decode_capacity: Option<f32>,
    monitor_label: Option<&str>,
    metadata_diagnostic_label: Option<&str>,
) -> Option<String> {
    if !is_hdr_source {
        return None;
    }

    let render = hdr_render_path_label(render_path);
    let color = color_space.map(hdr_color_space_label);
    let output = hdr_output_label(capabilities);

    let mut parts = match color {
        Some(color) => t!(
            "hdr.osd.tag_with_color",
            color = color,
            render = render,
            output = output
        )
        .to_string(),
        None => t!("hdr.osd.tag_without_color", render = render, output = output).to_string(),
    };
    if let Some(capacity) = ultra_hdr_decode_capacity {
        let capacity = format!("{capacity:.2}");
        parts.push_str(&t!("hdr.osd.jpeg_r_cap", capacity = capacity));
    }
    if let Some(label) = monitor_label.filter(|label| !label.is_empty()) {
        parts.push_str(" | ");
        parts.push_str(label);
    }
    if let Some(label) = metadata_diagnostic_label.filter(|label| !label.is_empty()) {
        parts.push_str(" | ");
        parts.push_str(label);
    }
    Some(parts)
}

fn hdr_render_path_label(render_path: HdrRenderPath) -> String {
    match render_path {
        HdrRenderPath::FloatImagePlane => t!("hdr.render_path.float_plane").to_string(),
        HdrRenderPath::FloatTilePlane => t!("hdr.render_path.float_tile_plane").to_string(),
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

fn hdr_color_space_label(color_space: HdrColorSpace) -> String {
    match color_space {
        HdrColorSpace::LinearSrgb => t!("hdr.color_space.linear_srgb").to_string(),
        HdrColorSpace::LinearScRgb => t!("hdr.color_space.linear_scrgb").to_string(),
        HdrColorSpace::Rec2020Linear => t!("hdr.color_space.rec2020_linear").to_string(),
        HdrColorSpace::Aces2065_1 => t!("hdr.color_space.aces2065_1").to_string(),
        HdrColorSpace::Xyz => t!("hdr.color_space.xyz").to_string(),
        HdrColorSpace::DisplayP3Linear => t!("hdr.color_space.display_p3_linear").to_string(),
        HdrColorSpace::Unknown => t!("hdr.color_space.unknown").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::hdr::capabilities::HdrCapabilities;
    use crate::hdr::status::{HdrRenderPath, hdr_osd_tag};
    use crate::hdr::types::HdrColorSpace;
    use rust_i18n::t;

    #[test]
    fn hdr_osd_tag_names_float_plane_and_sdr_output() {
        rust_i18n::set_locale("en");
        let render = t!("hdr.render_path.float_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let expected = t!("hdr.osd.tag_without_color", render = render, output = output).to_string();
        let tag = hdr_osd_tag(
            true,
            HdrRenderPath::FloatImagePlane,
            None,
            &HdrCapabilities::sdr("native HDR output not enabled"),
            None,
            None,
            None,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_names_float_tile_plane() {
        rust_i18n::set_locale("en");
        let render = t!("hdr.render_path.float_tile_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let expected = t!("hdr.osd.tag_without_color", render = render, output = output).to_string();
        let tag = hdr_osd_tag(
            true,
            HdrRenderPath::FloatTilePlane,
            None,
            &HdrCapabilities::sdr("native HDR output not enabled"),
            None,
            None,
            None,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_includes_known_input_color_space() {
        rust_i18n::set_locale("en");
        let color = t!("hdr.color_space.rec2020_linear").to_string();
        let render = t!("hdr.render_path.float_tile_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let expected =
            t!(
                "hdr.osd.tag_with_color",
                color = color,
                render = render,
                output = output
            )
            .to_string();
        let tag = hdr_osd_tag(
            true,
            HdrRenderPath::FloatTilePlane,
            Some(HdrColorSpace::Rec2020Linear),
            &HdrCapabilities::sdr("native HDR output not enabled"),
            None,
            None,
            None,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_includes_ultra_hdr_capacity_and_monitor() {
        rust_i18n::set_locale("en");
        let color = t!("hdr.color_space.rec2020_linear").to_string();
        let render = t!("hdr.render_path.float_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let mut expected = t!(
            "hdr.osd.tag_with_color",
            color = color,
            render = render,
            output = output
        )
        .to_string();
        expected.push_str(&t!("hdr.osd.jpeg_r_cap", capacity = "5.50"));
        expected.push_str(" | DISPLAY1");
        let tag = hdr_osd_tag(
            true,
            HdrRenderPath::FloatImagePlane,
            Some(HdrColorSpace::Rec2020Linear),
            &HdrCapabilities::sdr("native HDR output not enabled"),
            Some(5.5),
            Some("DISPLAY1"),
            None,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_includes_metadata_diagnostic_label() {
        rust_i18n::set_locale("en");
        let color = t!("hdr.color_space.linear_srgb").to_string();
        let render = t!("hdr.render_path.float_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let mut expected = t!(
            "hdr.osd.tag_with_color",
            color = color,
            render = render,
            output = output
        )
        .to_string();
        expected.push_str(" | metadata: EXR chromaticities");
        let tag = hdr_osd_tag(
            true,
            HdrRenderPath::FloatImagePlane,
            Some(HdrColorSpace::LinearSrgb),
            &HdrCapabilities::sdr("native HDR output not enabled"),
            None,
            None,
            Some("metadata: EXR chromaticities"),
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_warns_for_unknown_input_color_space() {
        rust_i18n::set_locale("en");
        let color = t!("hdr.color_space.unknown").to_string();
        let render = t!("hdr.render_path.float_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let expected =
            t!(
                "hdr.osd.tag_with_color",
                color = color,
                render = render,
                output = output
            )
            .to_string();
        let tag = hdr_osd_tag(
            true,
            HdrRenderPath::FloatImagePlane,
            Some(HdrColorSpace::Unknown),
            &HdrCapabilities::sdr("native HDR output not enabled"),
            None,
            None,
            None,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_is_hidden_for_non_hdr_images() {
        rust_i18n::set_locale("en");
        let tag = hdr_osd_tag(
            false,
            HdrRenderPath::SdrFallback,
            Some(HdrColorSpace::LinearSrgb),
            &HdrCapabilities::sdr("not an HDR image"),
            None,
            None,
            None,
        );

        assert_eq!(tag, None);
    }
}
