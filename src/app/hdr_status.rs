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

use crate::app::{ImageViewerApp, TransitionStyle};
use crate::hdr::status::{HdrRenderPath, hdr_osd_tag};
use crate::hdr::types::HdrColorSpace;

impl ImageViewerApp {
    pub(crate) fn current_hdr_render_path(&self) -> Option<HdrRenderPath> {
        let has_hdr_tiled_source = self
            .current_hdr_tiled_image
            .as_ref()
            .is_some_and(|current| current.source_for_index(self.current_index).is_some());
        let has_sdr_fallback = self.hdr_sdr_fallback_indices.contains(&self.current_index);

        let has_hdr_image = self
            .current_hdr_image
            .as_ref()
            .is_some_and(|current| current.image_for_index(self.current_index).is_some());

        let complex_transition_active = self.transition_start.is_some()
            && matches!(
                self.active_transition,
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
            );

        hdr_render_path_for_state(
            has_hdr_tiled_source,
            has_hdr_image,
            has_sdr_fallback,
            self.hdr_target_format,
            complex_transition_active,
        )
    }

    pub(crate) fn current_hdr_osd_tag(&self) -> Option<String> {
        let render_path = self.current_hdr_render_path()?;
        hdr_osd_tag(
            true,
            render_path,
            self.current_hdr_color_space(),
            &self.hdr_capabilities,
        )
    }

    fn current_hdr_color_space(&self) -> Option<HdrColorSpace> {
        if let Some(source) = self
            .current_hdr_tiled_image
            .as_ref()
            .and_then(|current| current.source_for_index(self.current_index))
        {
            return Some(source.color_space());
        }

        self.current_hdr_image
            .as_ref()
            .and_then(|current| current.image_for_index(self.current_index))
            .map(|image| image.color_space)
    }
}

fn hdr_render_path_for_state(
    has_hdr_tiled_source: bool,
    has_hdr_image: bool,
    has_sdr_fallback: bool,
    hdr_target_format: Option<wgpu::TextureFormat>,
    complex_transition_active: bool,
) -> Option<HdrRenderPath> {
    let has_renderable_target = hdr_target_format.is_some();
    if has_hdr_tiled_source && has_renderable_target {
        return Some(HdrRenderPath::FloatTilePlane);
    }

    if has_hdr_image && has_renderable_target && !complex_transition_active {
        return Some(HdrRenderPath::FloatImagePlane);
    }

    if has_hdr_image || has_hdr_tiled_source || has_sdr_fallback {
        Some(HdrRenderPath::SdrFallback)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::hdr_render_path_for_state;
    use crate::hdr::status::HdrRenderPath;

    #[test]
    fn hdr_tiled_source_reports_tile_plane_before_sdr_fallback() {
        assert_eq!(
            hdr_render_path_for_state(
                true,
                false,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                false
            ),
            Some(HdrRenderPath::FloatTilePlane)
        );
    }

    #[test]
    fn hdr_tiled_source_reports_sdr_fallback_without_hdr_target() {
        assert_eq!(
            hdr_render_path_for_state(true, false, true, None, false),
            Some(HdrRenderPath::SdrFallback)
        );
    }

    #[test]
    fn complex_transition_keeps_full_image_hdr_on_sdr_fallback() {
        assert_eq!(
            hdr_render_path_for_state(
                false,
                true,
                false,
                Some(wgpu::TextureFormat::Rgba16Float),
                true
            ),
            Some(HdrRenderPath::SdrFallback)
        );
    }

    #[test]
    fn hdr_render_path_uses_float_plane_even_when_shader_tone_maps_to_sdr_target() {
        assert_eq!(
            hdr_render_path_for_state(
                false,
                true,
                true,
                Some(wgpu::TextureFormat::Bgra8Unorm),
                false
            ),
            Some(HdrRenderPath::FloatImagePlane)
        );
    }

    #[test]
    fn hdr_render_path_matrix_keeps_float_routes_when_target_exists() {
        let cases = [
            (
                "static HDR",
                false,
                true,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                false,
                Some(HdrRenderPath::FloatImagePlane),
            ),
            (
                "tiled HDR",
                true,
                false,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                false,
                Some(HdrRenderPath::FloatTilePlane),
            ),
            (
                "static HDR on SDR target",
                false,
                true,
                true,
                Some(wgpu::TextureFormat::Bgra8Unorm),
                false,
                Some(HdrRenderPath::FloatImagePlane),
            ),
            (
                "tiled HDR without render target",
                true,
                false,
                true,
                None,
                false,
                Some(HdrRenderPath::SdrFallback),
            ),
        ];

        for (
            label,
            has_hdr_tiled_source,
            has_hdr_image,
            has_sdr_fallback,
            hdr_target_format,
            complex_transition_active,
            expected,
        ) in cases
        {
            assert_eq!(
                hdr_render_path_for_state(
                    has_hdr_tiled_source,
                    has_hdr_image,
                    has_sdr_fallback,
                    hdr_target_format,
                    complex_transition_active,
                ),
                expected,
                "{label}"
            );
        }
    }
}
