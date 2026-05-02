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

use crate::app::rendering::plan::{RenderShape, build_render_plan_for_state};
use crate::app::rendering::plane::PlaneBackendKind;
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

        hdr_render_path_for_viewer_plan(
            self.tile_manager.is_some(),
            has_hdr_tiled_source,
            has_hdr_image,
            has_sdr_fallback,
            self.hdr_target_format,
            complex_transition_active,
            self.hdr_monitor_state.selection(),
        )
    }

    pub(crate) fn current_hdr_osd_tag(&self) -> Option<String> {
        let render_path = self.current_hdr_render_path()?;
        hdr_osd_tag(
            true,
            render_path,
            self.current_hdr_color_space(),
            &self.hdr_capabilities,
            Some(self.ultra_hdr_decode_capacity),
            self.hdr_monitor_state
                .selection()
                .map(|selection| selection.label.as_str()),
            self.current_hdr_metadata_diagnostic_label(),
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

    fn current_hdr_metadata_diagnostic_label(&self) -> Option<&'static str> {
        let path = self.image_files.get(self.current_index)?;
        let ext = path.extension()?.to_str()?;
        if (ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"))
            && self
                .ultra_hdr_capacity_sensitive_indices
                .contains(&self.current_index)
        {
            Some("metadata: JPEG_R gain map")
        } else if ext.eq_ignore_ascii_case("hdr") {
            Some("metadata: Radiance EXPOSURE/COLORCORR")
        } else if ext.eq_ignore_ascii_case("exr") {
            Some("metadata: EXR chromaticities")
        } else {
            None
        }
    }
}

fn hdr_render_path_for_viewer_plan(
    tiled_canvas_active: bool,
    has_hdr_tiled_source: bool,
    has_hdr_image: bool,
    has_sdr_fallback: bool,
    hdr_target_format: Option<wgpu::TextureFormat>,
    complex_transition_active: bool,
    monitor_selection: Option<&crate::hdr::monitor::HdrMonitorSelection>,
) -> Option<HdrRenderPath> {
    let shape = if tiled_canvas_active {
        RenderShape::Tiled
    } else {
        RenderShape::Static
    };
    let has_hdr_plane = if shape == RenderShape::Tiled {
        has_hdr_tiled_source
    } else {
        has_hdr_image
    };
    let plan =
        build_render_plan_for_state(shape, has_hdr_plane, hdr_target_format, monitor_selection);

    if plan.backend == PlaneBackendKind::Hdr {
        return match shape {
            RenderShape::Tiled => Some(HdrRenderPath::FloatTilePlane),
            RenderShape::Static if !complex_transition_active => {
                Some(HdrRenderPath::FloatImagePlane)
            }
            RenderShape::Static => Some(HdrRenderPath::SdrFallback),
        };
    }

    if has_hdr_image || has_hdr_tiled_source || has_sdr_fallback {
        Some(HdrRenderPath::SdrFallback)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::hdr_render_path_for_viewer_plan;
    use crate::hdr::status::HdrRenderPath;

    #[test]
    fn hdr_tiled_source_reports_tile_plane_before_sdr_fallback() {
        assert_eq!(
            hdr_render_path_for_viewer_plan(
                true,
                true,
                false,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                false,
                None,
            ),
            Some(HdrRenderPath::FloatTilePlane)
        );
    }

    #[test]
    fn hdr_tiled_source_reports_sdr_fallback_without_hdr_target() {
        assert_eq!(
            hdr_render_path_for_viewer_plan(true, true, false, true, None, false, None),
            Some(HdrRenderPath::SdrFallback)
        );
    }

    #[test]
    fn complex_transition_keeps_full_image_hdr_on_sdr_fallback() {
        assert_eq!(
            hdr_render_path_for_viewer_plan(
                false,
                false,
                true,
                false,
                Some(wgpu::TextureFormat::Rgba16Float),
                true,
                None,
            ),
            Some(HdrRenderPath::SdrFallback)
        );
    }

    #[test]
    fn tone_mapped_sdr_surface_matches_render_plan_sdr_fallback_osd() {
        assert_eq!(
            hdr_render_path_for_viewer_plan(
                false,
                false,
                true,
                true,
                Some(wgpu::TextureFormat::Bgra8Unorm),
                false,
                None,
            ),
            Some(HdrRenderPath::SdrFallback)
        );
    }

    #[test]
    fn hdr_render_path_matrix_aligns_with_render_plan() {
        let cases = [
            (
                "static HDR native target",
                false,
                false,
                true,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                false,
                Some(HdrRenderPath::FloatImagePlane),
            ),
            (
                "tiled HDR native target",
                true,
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
                false,
                true,
                true,
                Some(wgpu::TextureFormat::Bgra8Unorm),
                false,
                Some(HdrRenderPath::SdrFallback),
            ),
            (
                "tiled HDR without render target",
                true,
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
            tiled_canvas_active,
            has_hdr_tiled_source,
            has_hdr_image,
            has_sdr_fallback,
            hdr_target_format,
            complex_transition_active,
            expected,
        ) in cases
        {
            assert_eq!(
                hdr_render_path_for_viewer_plan(
                    tiled_canvas_active,
                    has_hdr_tiled_source,
                    has_hdr_image,
                    has_sdr_fallback,
                    hdr_target_format,
                    complex_transition_active,
                    None,
                ),
                expected,
                "{label}"
            );
        }
    }
}
