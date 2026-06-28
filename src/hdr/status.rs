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

use super::types::{HdrColorSpace, HdrOutputMode};
use rust_i18n::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrRenderPath {
    FloatImagePlane,
    FloatTilePlane,
    SdrFallback,
}

pub struct HdrOsdTagParts<'a> {
    pub is_hdr_source: bool,
    pub render_path: HdrRenderPath,
    pub color_space: Option<HdrColorSpace>,
    pub output_mode: HdrOutputMode,
    pub native_presentation_enabled: bool,
    pub ultra_hdr_decode_capacity: Option<f32>,
    pub monitor_label: Option<&'a str>,
    pub exposure_ev: f32,
}

pub fn hdr_osd_tag_from_parts(input: HdrOsdTagParts<'_>) -> Option<String> {
    if !input.is_hdr_source {
        return None;
    }

    let render = hdr_render_path_label(input.render_path);
    let color = input.color_space.map(hdr_color_space_label);
    let output = hdr_output_label_from_parts(input.output_mode, input.native_presentation_enabled);

    let mut parts = match color {
        Some(color) => t!(
            "hdr.osd.tag_with_color",
            color = color,
            render = render,
            output = output
        )
        .to_string(),
        None => t!(
            "hdr.osd.tag_without_color",
            render = render,
            output = output
        )
        .to_string(),
    };
    if let Some(capacity) = input.ultra_hdr_decode_capacity {
        let capacity = format!("{capacity:.2}");
        parts.push_str(&t!("hdr.osd.jpeg_r_cap", capacity = capacity));
    }
    if let Some(label) = input.monitor_label.filter(|label| !label.is_empty()) {
        parts.push_str(" | ");
        parts.push_str(label);
    }
    parts.push_str(" · ");
    parts.push_str(&format_hdr_osd_exposure_ev(input.exposure_ev));
    Some(parts)
}

fn format_hdr_osd_exposure_ev(ev: f32) -> String {
    let v = if ev.is_finite() { ev } else { 0.0 };
    format!("{v:+.1} EV")
}

fn hdr_render_path_label(render_path: HdrRenderPath) -> String {
    match render_path {
        HdrRenderPath::FloatImagePlane => t!("hdr.render_path.float_plane").to_string(),
        HdrRenderPath::FloatTilePlane => t!("hdr.render_path.float_tile_plane").to_string(),
        HdrRenderPath::SdrFallback => t!("hdr.render_path.sdr_fallback").to_string(),
    }
}

pub fn hdr_output_label_from_parts(
    output_mode: HdrOutputMode,
    native_presentation_enabled: bool,
) -> String {
    if native_presentation_enabled {
        match output_mode {
            HdrOutputMode::WindowsScRgb => t!("hdr.output.windows_scrgb").to_string(),
            HdrOutputMode::MacOsEdr => t!("hdr.output.macos_edr").to_string(),
            HdrOutputMode::WaylandHdr => t!("hdr.output.wayland_hdr").to_string(),
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
    use crate::hdr::status::{HdrRenderPath, hdr_osd_tag_from_parts};
    use crate::hdr::types::{HdrColorSpace, HdrOutputMode};
    use rust_i18n::t;

    #[test]
    fn hdr_osd_tag_names_float_plane_and_sdr_output() {
        rust_i18n::set_locale("en");
        let render = t!("hdr.render_path.float_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let mut expected = t!(
            "hdr.osd.tag_without_color",
            render = render,
            output = output
        )
        .to_string();
        expected.push_str(" · +0.0 EV");
        let tag = hdr_osd_tag_from_parts(
            true,
            HdrRenderPath::FloatImagePlane,
            None,
            HdrOutputMode::SdrToneMapped,
            false,
            None,
            None,
            0.0,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_names_float_tile_plane() {
        rust_i18n::set_locale("en");
        let render = t!("hdr.render_path.float_tile_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let mut expected = t!(
            "hdr.osd.tag_without_color",
            render = render,
            output = output
        )
        .to_string();
        expected.push_str(" · +0.0 EV");
        let tag = hdr_osd_tag_from_parts(
            true,
            HdrRenderPath::FloatTilePlane,
            None,
            HdrOutputMode::SdrToneMapped,
            false,
            None,
            None,
            0.0,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_includes_known_input_color_space() {
        rust_i18n::set_locale("en");
        let color = t!("hdr.color_space.rec2020_linear").to_string();
        let render = t!("hdr.render_path.float_tile_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let mut expected = t!(
            "hdr.osd.tag_with_color",
            color = color,
            render = render,
            output = output
        )
        .to_string();
        expected.push_str(" · +0.0 EV");
        let tag = hdr_osd_tag_from_parts(
            true,
            HdrRenderPath::FloatTilePlane,
            Some(HdrColorSpace::Rec2020Linear),
            HdrOutputMode::SdrToneMapped,
            false,
            None,
            None,
            0.0,
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
        expected.push_str(" | DISPLAY1 · +0.0 EV");
        let tag = hdr_osd_tag_from_parts(
            true,
            HdrRenderPath::FloatImagePlane,
            Some(HdrColorSpace::Rec2020Linear),
            HdrOutputMode::SdrToneMapped,
            false,
            Some(5.5),
            Some("DISPLAY1"),
            0.0,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_warns_for_unknown_input_color_space() {
        rust_i18n::set_locale("en");
        let color = t!("hdr.color_space.unknown").to_string();
        let render = t!("hdr.render_path.float_plane").to_string();
        let output = t!("hdr.output.sdr_tone_mapped").to_string();
        let mut expected = t!(
            "hdr.osd.tag_with_color",
            color = color,
            render = render,
            output = output
        )
        .to_string();
        expected.push_str(" · +0.0 EV");
        let tag = hdr_osd_tag_from_parts(
            true,
            HdrRenderPath::FloatImagePlane,
            Some(HdrColorSpace::Unknown),
            HdrOutputMode::SdrToneMapped,
            false,
            None,
            None,
            0.0,
        );

        assert_eq!(tag.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hdr_osd_tag_formats_exposure_suffix() {
        let tag = hdr_osd_tag_from_parts(
            true,
            HdrRenderPath::FloatImagePlane,
            None,
            HdrOutputMode::SdrToneMapped,
            false,
            None,
            None,
            3.0,
        )
        .expect("HDR tag Some");
        assert!(tag.ends_with(" · +3.0 EV"));
    }

    #[test]
    fn hdr_osd_tag_is_hidden_for_non_hdr_images() {
        rust_i18n::set_locale("en");
        let tag = hdr_osd_tag_from_parts(
            false,
            HdrRenderPath::SdrFallback,
            Some(HdrColorSpace::LinearSrgb),
            HdrOutputMode::SdrToneMapped,
            false,
            None,
            None,
            99.0,
        );

        assert_eq!(tag, None);
    }
}
